//! The app's shell: everything around window management that a Mac app is expected to do.
//! About, Settings, the welcome, update checks and installs, launch at login, the approvals,
//! and Apple's Stage Manager (stand down, offer to turn it off). Main thread only.
//!
//! Updates (8C): a newer release is offered as "Install and Relaunch / Later / Skip This
//! Version" (or Download where this copy cannot replace itself). Install downloads and
//! verifies it in the background (`platform::updater`), waits for a quiet moment (never
//! mid-drag or mid-transition; an automatic install also waits for the user to be away),
//! then quits through the app's quit path, which starts the installer helper last. A ready
//! update also installs at the user's own Quit, without reopening the app
//! ([`install_on_quit`]); a launch that finds an update an earlier run left behind checks
//! again within its first minute instead of waiting a day. Who asked ([`Asker`]) decides
//! whether an alert takes the focus and where it shows: a click in Settings gets sheets on
//! the Settings window ([`alert::place`]).
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
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSPasteboard, NSPasteboardTypeString, NSRunningApplication, NSWorkspace};
use objc2_core_graphics::{CGDisplayIsAsleep, CGEventSource, CGEventSourceStateID, CGEventType, CGMainDisplayID};
use objc2_foundation::{MainThreadMarker, NSArray, NSBundle, NSString, NSTimer, NSURL};

use super::alert::{self, AlertSpec, Place};
use super::app_menu::{self, AppEvent};
use super::settings_window::{self, Change, Pane, SettingsValues};
use super::status_menu::MenuAction;
use super::welcome::{self, WelcomeAction};
use super::about;
use crate::app_state::{AppState, now_secs};
use crate::config::schema::REFERENCE_URL;
use crate::config::{self, Config, FitMode, LoginStart, edit};
use crate::engine::reopen_list::{self, BackStep, Policy, ReopenSwitch};
use crate::engine::stand_down::Signal;
use crate::platform::login_item::{self, Service};
use crate::platform::native_settings::{self, RawSettings};
use crate::platform::permissions::{self, Approval, PermissionState};
use crate::platform::wm_thread::{WmCommand, WmHandle};
use crate::platform::updater::{self, PrepareError, Prepared};
use crate::platform::{capture, instance, paths, session, update_check};
use crate::self_update::{self, Moment, QuitFacts, Refusal, StaleWork};
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
/// How often a ready update looks for a quiet moment.
const QUIET_TICK: f64 = 1.0;
/// When the note of an update that did not go in shows (after the launch settles).
const NOTE_DELAY: f64 = 5.0;

/// Calls into the app, run after the current event.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Show or hide the menu bar icon.
    pub set_menu_icon: fn(bool),
    /// Redraw the status menu.
    pub refresh_menu: fn(),
    /// Run the app's poll now (Apple's settings were written).
    pub poll_now: fn(),
    /// Quit through the app's quit path (every window back first): an update installs.
    pub quit: fn(),
}

/// Who asked for an update check, an install or Apple's turn-off: whether its alerts take
/// the focus, and where they show (round settings-sheet). Ordered: a click joining a run
/// raises it to the later asker, a click in Settings the highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Asker {
    /// Nobody: an automatic check or install; its alerts never take the focus.
    Nobody,
    /// A click outside the Settings window (the menu bar, the welcome): free-standing alerts.
    User,
    /// A click in the Settings window: its alerts are sheets on it while it is up.
    Settings,
}

impl Asker {
    /// A user asked (the result shows, the alert takes the focus).
    pub fn asked(self) -> bool {
        self != Asker::Nobody
    }

    /// A menu bar click: one in Settings when Settings is what the user is looking at
    /// (frontmost and key), else one outside it.
    pub fn from_menu(settings_front: bool) -> Asker {
        if settings_front { Asker::Settings } else { Asker::User }
    }
}

/// Where an update install stands.
enum Install {
    /// Nothing under way.
    Idle,
    /// Downloading and verifying `version`; `by`: who started it (or joined it).
    Preparing { version: updates::Version, by: Asker },
    /// Verified and staged; waiting for a quiet moment. `told`: the wait for an unlock (or
    /// the display's wake) was logged, once per ready update.
    Ready { prepared: Prepared, by: Asker, timer: Retained<NSTimer>, told: bool },
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
    /// The update check in flight: its number, and who asked for it (a user's: its result
    /// shows).
    update: Option<(u64, Asker)>,
    update_serial: u64,
    install: Install,
    /// An earlier run left a downloaded update behind: an automatic check is owed now, not in
    /// a day (until a check gets an answer).
    left_behind: bool,
    /// The launch's first automatic check came (a left-behind update found later checks at
    /// once).
    first_check_done: bool,
    /// The activation policy through a logout (round reopen-list).
    reopen: ReopenSwitch,
    /// Counts the activation policy's switches: a pending retry of the switch back to
    /// accessory runs only while no newer switch came ([`Shell::back_to_accessory`]).
    policy_serial: u64,
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
    parse_read(std::fs::read_to_string(paths::config_file()))
}

/// A read of the config file, parsed: `None` when it could not be read (missing, not UTF-8,
/// a folder) or does not parse. Only a readable file that is empty means the defaults
/// (review R2).
fn parse_read(read: std::io::Result<String>) -> Option<Config> {
    Config::parse(&read.ok()?).ok().map(|p| p.config)
}

/// What a change of `[app]` asks of the shell: the menu bar icon to show, and the login item
/// to set (each `None` when unchanged; `login_item` also when `login_start` changed while
/// launch at login is on, so the registration moves to the new way).
#[derive(Debug, PartialEq, Eq)]
struct AppEffects {
    menu_icon: Option<bool>,
    login_item: Option<bool>,
}

fn app_effects(before: &config::AppConfig, after: &config::AppConfig) -> AppEffects {
    let changed = |b: bool, a: bool| (b != a).then_some(a);
    let moved = after.launch_at_login && before.login_start != after.login_start;
    AppEffects {
        menu_icon: changed(before.show_menu_bar_icon, after.show_menu_bar_icon),
        login_item: changed(before.launch_at_login, after.launch_at_login).or(moved.then_some(true)),
    }
}

/// The login service `[app] login_start` names.
fn login_way(app: &config::AppConfig) -> Service {
    match app.login_start {
        LoginStart::LoginItem => Service::MainApp,
        LoginStart::BackgroundItem => Service::Agent,
    }
}

/// The first launch's registration of launch at login (`on`: `[app] launch_at_login`):
/// whether it is done, so later launches leave it to the user's choice. One that failed is
/// not (the 1.7 review: it was marked done and never tried again).
fn first_login_set_up(on: bool, register: impl FnOnce() -> Result<(), String>) -> bool {
    !on || register().is_ok()
}

/// The way an install's launch at login was last set to (`AppState::login_start`); before
/// round reopen-list, the agent once round early-start moved it, else the login item.
fn login_way_done(state: &AppState) -> Service {
    match &state.login_start {
        Some(key) => Service::from_key(key),
        None if state.login_agent_moved => Service::Agent,
        None => Service::MainApp,
    }
}

/// A test run (`STAGE_MANAGER_TEST` or `STAGE_MANAGER_HOME`): the policy never changes,
/// unless the live suite asks ([`test_joins_reopen_list`]).
fn test_run() -> bool {
    std::env::var_os("STAGE_MANAGER_TEST").is_some() || paths::custom_home().is_some()
}

/// The live suite's own copy joins the reopen list at its simulated logout:
/// `STAGE_MANAGER_TEST_REOPEN_LIST`, honoured only with `STAGE_MANAGER_TEST`.
fn test_joins_reopen_list() -> bool {
    std::env::var_os("STAGE_MANAGER_TEST").is_some() && std::env::var_os("STAGE_MANAGER_TEST_REOPEN_LIST").is_some()
}

/// One try of the switch back to accessory (`resync`: regular first, [`BackStep::Resync`]);
/// returns AppKit's reading and macOS's (the running app's, as the Dock has it). Nothing is
/// activated and no window shown.
fn try_accessory(mtm: MainThreadMarker, resync: bool) -> (bool, bool) {
    let app = NSApplication::sharedApplication(mtm);
    if resync {
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    }
    // The answer is no measure: a switch to the policy AppKit reads answers NO (the probe).
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let accessory = NSApplicationActivationPolicy::Accessory;
    (app.activationPolicy() == accessory, NSRunningApplication::currentApplication().activationPolicy() == accessory)
}

/// Whether the launch is macOS's reopen after a logout that listed the app: `listed_at`
/// (the logout's Unix seconds, [`AppState::reopen_listed`]) came before the login, which
/// began `since_login` before `now`.
fn listed_before_login(listed_at: Option<u64>, now: u64, since_login: Option<Duration>) -> bool {
    match (listed_at, since_login) {
        (Some(at), Some(d)) => at <= now.saturating_sub(d.as_secs()),
        _ => false,
    }
}

/// Starts the shell. Call once the status menu exists.
pub fn install(mtm: MainThreadMarker, wm: WmHandle, hooks: Hooks, permissions: PermissionState, raw: RawSettings) {
    // A file that does not load: its last good copy, as the WM thread uses (review C1).
    let config = config::last_good::read_in_force(&paths::config_file());
    let state = AppState::load(&paths::app_state_file());
    app_menu::install(mtm, |event| match event {
        AppEvent::Launched(login_item_event) => {
            with(|s| s.launched(login_item_event));
        }
        AppEvent::Reopen => {
            // At login macOS reopened this copy (or the agent started it); the login item then
            // reaches it as a reopen, which nobody asked for (rounds early-start, reopen-list).
            let since_login = login_item::since_login();
            if instance::reopen_shows_settings(since_login) {
                tracing::info!("opened again while running: showing Settings");
                show_settings();
            } else {
                tracing::info!(login_ago_s = since_login.map(|d| d.as_secs()), "opened again moments after the login (the login item or macOS reopening it): no Settings");
            }
        }
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
        install: Install::Idle,
        left_behind: false,
        first_check_done: false,
        reopen: ReopenSwitch::default(),
        policy_serial: 0,
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
        let way = login_way(&self.config.app);
        if !self.state.login_item_set_up {
            match login_item::refusal() {
                None if first_login_set_up(self.config.app.launch_at_login, || login_item::set(true, way)) => {
                    self.state.login_item_set_up = true;
                    self.state.login_start = Some(way.key().into());
                    self.save_state();
                }
                None => tracing::warn!("launch at login: the first registration failed; tried again at the next launch"),
                Some(why) => tracing::debug!("login item not set up: {why}"),
            }
        } else if login_way_done(&self.state) != way {
            // The way changed (1.6.x's agent, now the login item; or the key): moved once.
            if login_item::move_to(way) {
                self.state.login_start = Some(way.key().into());
                self.save_state();
            }
        } else {
            login_item::tidy(way);
        }
        let first_check = RcBlock::new(|_t: NonNull<NSTimer>| first_check());
        let tick = RcBlock::new(|_t: NonNull<NSTimer>| auto_check());
        if self.config.app.check_for_updates && unattended_allowed() {
            updater::warm();
        }
        // A staged copy an earlier update left beside the app (it quit while an update
        // waited, or its helper was killed) goes, unless a helper runs now (review 8C #6) or
        // automatic installs are on (it then serves the next check, verified again, in place
        // of the same download at every launch); the update it held is checked for again
        // within the first minute (the owner's go 11-09).
        let auto = self.config.app.check_for_updates && self.config.app.install_updates;
        updater::clean_stale_work(auto, |found| later(move || stale_work_found(found)));
        let note = RcBlock::new(|_t: NonNull<NSTimer>| show_update_note());
        // SAFETY: the blocks run on the main run loop, which keeps the timers; we keep them too.
        unsafe {
            self._timers.push(NSTimer::scheduledTimerWithTimeInterval_repeats_block(FIRST_CHECK_DELAY, false, &first_check));
            self._timers.push(NSTimer::scheduledTimerWithTimeInterval_repeats_block(CHECK_TICK, true, &tick));
            self._timers.push(NSTimer::scheduledTimerWithTimeInterval_repeats_block(NOTE_DELAY, false, &note));
        }
    }

    /// AppKit finished launching (its open event read, `login_item_event`): the launch's
    /// name, now that a login item's launch and macOS's reopen can be told apart
    /// ([`login_item::opened_as`]); the reopen list's mark is read and cleared.
    fn launched(&mut self, login_item_event: Option<bool>) {
        let since_login = login_item::since_login();
        let listed = listed_before_login(self.state.reopen_listed, now_secs(), since_login);
        tracing::info!(
            started = login_item::opened_as(login_item::started(), login_item_event, listed),
            login_item_event,
            listed_at_logout = listed,
            login_ago_s = since_login.map(|d| d.as_secs()),
            "launched: as macOS opened it"
        );
        if self.state.reopen_listed.take().is_some() {
            self.save_state();
        }
    }

    /// Sets the app's activation policy for the reopen list (round reopen-list): no window
    /// shown, nothing activated. Regular marks the logout in `app-state.json`, so the next
    /// launch can tell macOS's reopen; accessory clears it. Done is the policy read after
    /// the switch, not AppKit's answer; the switch back is tried again until it holds
    /// ([`reopen_list::back_step`]), and a newer switch ends those tries.
    fn set_policy(&mut self, policy: Policy, why: &'static str) {
        self.policy_serial += 1;
        match policy {
            Policy::Regular => {
                let app = NSApplication::sharedApplication(self.mtm);
                let answered = app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
                let done = app.activationPolicy() == NSApplicationActivationPolicy::Regular;
                tracing::info!(policy = ?policy, done, answered, why, "reopen list: activation policy");
                self.state.reopen_listed = done.then(now_secs);
                self.save_state();
            }
            Policy::Accessory => {
                self.state.reopen_listed = None;
                self.save_state();
                self.back_to_accessory(self.policy_serial, 1, false, why);
            }
        }
    }

    /// Try number `tries` of the switch back to accessory, while no newer switch came
    /// (`serial`); the outcome is logged once, when it holds or no try is left.
    fn back_to_accessory(&mut self, serial: u64, tries: usize, resync: bool, why: &'static str) {
        if serial != self.policy_serial {
            tracing::debug!(tries, "reopen list: the switch back to accessory is over: a newer switch came");
            return;
        }
        let (appkit, listed) = try_accessory(self.mtm, resync);
        let (wait, resync) = match reopen_list::back_step(tries, appkit, listed) {
            step @ (BackStep::Done | BackStep::GaveUp) => {
                let done = step == BackStep::Done;
                tracing::info!(policy = ?Policy::Accessory, done, tries, appkit_accessory = appkit, listed_accessory = listed, why, "reopen list: activation policy");
                return;
            }
            BackStep::Again(wait) => (wait, false),
            BackStep::Resync(wait) => (wait, true),
        };
        tracing::debug!(tries, appkit_accessory = appkit, listed_accessory = listed, wait_ms = wait.as_millis() as u64, resync, "reopen list: the switch back to accessory did not hold; trying again");
        let when = dispatch2::DispatchTime::try_from(wait).unwrap_or(dispatch2::DispatchTime::NOW);
        let _ = dispatch2::DispatchQueue::main().after(when, move || {
            with(|s| s.back_to_accessory(serial, tries + 1, resync, why));
        });
    }

    fn welcome_rows(&self, apple_row: bool) -> Vec<welcome::WelcomeRow> {
        welcome::rows(&self.permissions, self.capture_on(), apple_row, self.apple_on())
    }

    fn show_welcome(&self) {
        let rows = self.welcome_rows(self.apple_on());
        welcome::show(self.mtm, rows, |action| {
            later(move || match action {
                WelcomeAction::Allow(a) => allow(a),
                WelcomeAction::TurnOffApple => turn_off_apple(Asker::User),
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
                // Only Settings writes here: its sheet.
                present(
                    self.mtm,
                    Asker::Settings,
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
            // No new reading will come: the control goes back to the value in force.
            self.refresh_settings();
        }
        let hooks = self.hooks;
        later(move || (hooks.poll_now)());
    }

    fn on_change(&mut self, change: Change) {
        match change {
            Change::LaunchAtLogin(on) => {
                self.write_config("app.launch_at_login", if on { "true" } else { "false" });
                let _ = login_item::set(on, login_way(&self.config.app));
                self.refresh_settings();
            }
            Change::OpenLoginItems => login_item::open_login_items(),
            Change::MenuBarIcon(on) => {
                self.write_config("app.show_menu_bar_icon", if on { "true" } else { "false" });
                let hooks = self.hooks;
                later(move || (hooks.set_menu_icon)(on));
            }
            Change::AutoUpdates(on) => self.write_config("app.check_for_updates", if on { "true" } else { "false" }),
            Change::InstallUpdates(on) => self.write_config("app.install_updates", if on { "true" } else { "false" }),
            Change::CheckNow => later(|| check(Asker::Settings)),
            Change::TurnOffApple => later(|| turn_off_apple(Asker::Settings)),
            Change::ShowRecentApps(on) => self.write_apple("AutoHide", i64::from(!on)),
            Change::AllAtOnce(all) => self.write_apple("AppWindowGroupingBehavior", i64::from(all)),
            Change::OpenDesktopAndDock => open_url(DESKTOP_DOCK_URL),
            Change::RestoreAfterRestart(on) => self.write_config("stages.restore_after_restart", if on { "true" } else { "false" }),
            Change::NewAppsJoinCurrent(on) => self.write_config("stages.new_apps_join_current", if on { "true" } else { "false" }),
            Change::MergeOnDrop(on) => self.write_config("strip.merge_on_drop", if on { "true" } else { "false" }),
            Change::ShiftBringsHere(on) => self.write_config("sugar.shift_brings_here", if on { "true" } else { "false" }),
            Change::StripOrder(order) => self.write_config("strip.order", &format!("\"{}\"", order.name())),
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
            Change::Pane(pane) => {
                // The app's own state, never config.toml: shown again at the next opening.
                if self.state.settings_pane.as_deref() != Some(pane.id()) {
                    self.state.settings_pane = Some(pane.id().to_string());
                    self.save_state();
                }
            }
        }
    }
}

/// Shows the Settings window in front (menu, ⌘,, a second launch, the welcome).
/// A logout signal, on the main thread as it arrives (round reopen-list): at the first of a
/// logout the app turns regular, so macOS reopens it with the other apps at login
/// ([`reopen_list`]); a cancel turns it back.
pub fn logout_signal(signal: Signal, at: Instant) {
    with(|s| {
        let key = s.config.app.reopen_with_apps;
        let policy = s.reopen.signal(signal, at, || {
            let login = login_item::status();
            let ok = reopen_list::allowed(key, login.starts_at_login(), test_run(), test_joins_reopen_list());
            if !ok {
                tracing::info!(reopen_with_apps = key, login = ?login, test = test_run(), "reopen list: not joined at this logout");
            }
            ok
        });
        match policy {
            Some(Policy::Regular) => s.set_policy(Policy::Regular, "a logout, restart or shutdown is coming: listed with the apps macOS reopens"),
            Some(Policy::Accessory) => s.set_policy(Policy::Accessory, "the logout was cancelled"),
            None => {}
        }
    });
}

/// An app quit (any, on the main thread as its notice arrives): during a logout that listed
/// the app, the logout is still under way ([`ReopenSwitch::app_quit`]).
pub fn app_quit(at: Instant) {
    with(|s| s.reopen.app_quit(at));
}

pub fn show_settings() {
    let Some((mtm, values, pane)) = with(|s| {
        let pane = s.state.settings_pane.as_deref().and_then(Pane::from_id).unwrap_or_default();
        (s.mtm, s.settings_values(), pane)
    }) else {
        return;
    };
    settings_window::show(mtm, values, pane, |change| {
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
        MenuAction::CheckForUpdates => check(menu_asker()),
        MenuAction::Settings => show_settings(),
        MenuAction::Allow(a) => allow(a),
        MenuAction::TurnOffAppleStageManager => turn_off_apple(menu_asker()),
        MenuAction::MarkProblem | MenuAction::Quit => {}
    }
}

/// Who a menu bar click is: one in Settings while Settings is frontmost and key.
fn menu_asker() -> Asker {
    Asker::from_menu(MainThreadMarker::new().is_some_and(settings_window::is_front))
}

/// Shows `spec` where `by` puts it: a sheet on the Settings window when the click came from
/// there and it is up with no other sheet of ours; free-standing otherwise ([`alert::place`]).
fn present(mtm: MainThreadMarker, by: Asker, spec: AlertSpec, on_button: impl Fn(usize) + 'static) {
    // The alert it replaces goes first: a sheet replaced under its key leaves room for this one.
    alert::close(spec.key);
    let window = settings_window::shown_window();
    let sheet_up = window.as_deref().is_some_and(alert::has_sheet);
    match (alert::place(by == Asker::Settings, window.is_some(), sheet_up), window) {
        (Place::Sheet, Some(w)) => alert::show_sheet(mtm, spec, &w, on_button),
        _ => alert::show(mtm, spec, on_button),
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
    with(|s| {
        if let Some(policy) = s.reopen.tick(Instant::now()) {
            s.set_policy(policy, "no quit came in time: the logout was cancelled");
        }
    });
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

/// The config was reloaded (by the WM thread): `config` is the one in force there (the last
/// good one after an error), so the shell never reads a different version of the file.
pub fn config_changed(error: Option<String>, config: Config) {
    with(|s| {
        let effects = app_effects(&s.config.app, &config.app);
        s.config_error = error;
        s.config = config;
        if let Some(icon) = effects.menu_icon {
            let hooks = s.hooks;
            later(move || (hooks.set_menu_icon)(icon));
        }
        if let Some(on) = effects.login_item
            && login_item::refusal().is_none()
        {
            // An edit of the file (an agent's) turns the login item on or off too, or moves it
            // to the way `login_start` names.
            let way = login_way(&s.config.app);
            if login_item::set(on, way).is_ok() {
                s.state.login_start = Some(way.key().into());
                s.save_state();
            }
        }
        s.refresh_settings();
    });
}

/// Whether window pictures are on (Screen Recording is asked for), for the menu.
pub fn capture_on() -> bool {
    with(|s| s.capture_on()).unwrap_or(true)
}

/// An automatic update check, when allowed, on, and a day has passed (or an update an
/// earlier run left behind is owed a check).
fn auto_check() {
    let due = with(|s| {
        s.config.app.check_for_updates && self_update::check_due(s.left_behind, updates::due(s.state.last_update_check, now_secs()))
    })
    .unwrap_or(false);
    if due && unattended_allowed() {
        check(Asker::Nobody);
    }
}

/// The launch's first automatic check ([`FIRST_CHECK_DELAY`] after the launch).
fn first_check() {
    with(|s| s.first_check_done = true);
    auto_check();
}

/// What the launch found beside the installed app (main thread): an update an earlier run
/// downloaded and verified that never went in is checked for again at the first check, or at
/// once when that came already; never trusted from the old folder (it is gone, or kept for
/// an automatic install and judged whole again before it is used).
fn stale_work_found(found: StaleWork) {
    if !self_update::checks_early(found) {
        return;
    }
    let Some(first_done) = with(|s| {
        s.left_behind = true;
        s.first_check_done
    }) else {
        return;
    };
    tracing::info!(?found, "update: an earlier run left a downloaded update behind; checking again within the first minute");
    if first_done {
        auto_check();
    }
}

/// How long a check may stay unanswered before it counts as failed (NSURLSession's own
/// timeout is 20 s; a completion that never comes must not wedge every later check).
const CHECK_WATCHDOG: Duration = Duration::from_secs(25);

/// Checks GitHub for a newer release; a user's check always answers (`by`). A user's check
/// while one runs makes that one's result show, where this one's would.
pub fn check(by: Asker) {
    let manual = by.asked();
    let started = with(|s| match s.update.as_mut() {
        Some((_, asker)) => {
            *asker = (*asker).max(by);
            None
        }
        None => {
            s.update_serial += 1;
            s.update = Some((s.update_serial, by));
            Some(s.update_serial)
        }
    })
    .flatten();
    let Some(serial) = started else {
        tracing::debug!(manual, "an update check is already running; its result will show");
        return;
    };
    tracing::info!(manual, "checking for updates");
    // The app's own signature is read meanwhile, off this thread (once; at launch already
    // when automatic checks are on): the answer's offer then runs no `codesign` here.
    updater::warm();
    update_check::check(move |outcome| later(move || on_checked(serial, outcome)));
    let when = dispatch2::DispatchTime::try_from(CHECK_WATCHDOG).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, move || on_checked(serial, Outcome::Failed("GitHub did not answer".into())));
}

/// The check numbered `serial` ended (its answer, or the watchdog: whichever comes first).
fn on_checked(serial: u64, outcome: Outcome) {
    let Some(Some((mtm, skipped, by, auto_install, owed))) = with(|s| {
        let (running, by) = s.update?;
        if running != serial {
            return None;
        }
        s.update = None;
        let mut owed = false;
        if !matches!(outcome, Outcome::Failed(_)) {
            s.state.last_update_check = Some(now_secs());
            owed = std::mem::take(&mut s.left_behind);
            s.save_state();
        }
        Some((s.mtm, s.state.skipped_version.clone(), by, s.config.app.install_updates, owed))
    }) else {
        return;
    };
    let manual = by.asked();
    let current = updates::current();
    match outcome {
        Outcome::Available(release) => {
            tracing::info!(version = %release.version, manual, "an update is available");
            let worth = updates::worth_telling(&release, skipped.as_deref());
            if !manual && worth && auto_install && updater::can_install().is_ok() {
                // "Install updates automatically": no prompt; installed once the user is away.
                begin_install(release, Asker::Nobody);
            } else if manual || worth {
                offer(mtm, release, &current, by);
            }
        }
        Outcome::UpToDate => {
            tracing::info!(manual, "up to date");
            if manual {
                let (title, text) = updates::up_to_date_text(&current);
                present(mtm, by, AlertSpec { key: "update", title, text, buttons: vec!["OK".into()], cancel: Some(0), warning: false, activate: true }, |_| {});
            }
        }
        Outcome::Failed(e) => {
            if manual {
                tracing::warn!("update check failed: {e}");
                present(
                    mtm,
                    by,
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
    // The check a left-behind update was owed got its answer: a staged copy the launch kept
    // goes unless an install took it up just now (it uses the copy, or replaces it).
    if owed && with(|s| matches!(s.install, Install::Idle)) == Some(true) {
        updater::drop_stale_work();
    }
}

/// The update alert's spec (also the preview's): Install and Relaunch when this copy can
/// replace itself (`install` is `Ok`), else Download with the reason.
pub fn offer_spec(release: &Release, current: &updates::Version, install: &Result<std::path::PathBuf, Refusal>) -> AlertSpec {
    let why = install.as_ref().err().map(Refusal::sentence);
    let (title, text) = updates::available_text(release, current, why.as_deref().map_or(Ok(()), Err));
    let first = if install.is_ok() { "Install and Relaunch" } else { "Download" };
    AlertSpec {
        key: "update",
        title,
        text,
        buttons: vec![first.into(), "Later".into(), "Skip This Version".into()],
        cancel: Some(1),
        warning: false,
        activate: true,
    }
}

/// Offers `release`. A user's check brings the alert to the front; an automatic one never
/// takes the focus from what the user is doing (R8 V2): the alert comes up above other
/// windows without activating the app, and keyboard focus stays where it was.
fn offer(mtm: MainThreadMarker, release: Release, current: &updates::Version, by: Asker) {
    let install = updater::can_install();
    let mut spec = offer_spec(&release, current, &install);
    spec.activate = by.asked();
    let verifiable = match &install {
        Ok(_) => true,
        Err(r) => r.opens_disk_image(),
    };
    // The click answers for whoever asked; an automatic offer's Install is a user's (outside
    // Settings: the alert stood free).
    let clicked = by.max(Asker::User);
    present(mtm, by, spec, move |button| match button {
        0 if verifiable => {
            let r = release.clone();
            later(move || begin_install(r, clicked));
        }
        0 => open_url(release.download_url()),
        2 => skip_version(&release.version),
        _ => {}
    });
}

/// "Skip This Version": automatic checks stay quiet about `version` and older.
fn skip_version(version: &updates::Version) {
    let v = version.to_string();
    tracing::info!(version = %v, "update skipped");
    later(move || {
        with(|s| {
            s.state.skipped_version = Some(v);
            s.save_state();
        });
    });
}

/// Test builds only: with this set to `1`, `debug-update` readies the update as an automatic
/// install does (it then waits for the user to be away, or for the user's Quit).
#[cfg(feature = "update-test")]
pub const TEST_AUTO_ENV: &str = "STAGE_MANAGER_UPDATE_AUTO";

/// Test builds only (`update-test`, the `debug-update` ctl command): checks for an update
/// and installs it as "Install and Relaunch" does, with no alert (as an automatic install
/// does with [`TEST_AUTO_ENV`]).
#[cfg(feature = "update-test")]
pub fn debug_install() {
    let auto = std::env::var(TEST_AUTO_ENV).is_ok_and(|v| v == "1");
    update_check::check(move |outcome| {
        later(move || match outcome {
            Outcome::Available(release) => {
                tracing::warn!(version = %release.version, auto, "debug-update: installing as Install and Relaunch (or, auto, an automatic install) does");
                begin_install(release, if auto { Asker::Nobody } else { Asker::User });
            }
            other => tracing::warn!(?other, "debug-update: nothing to install"),
        })
    });
}

/// Downloads and verifies `release`, then installs it at a quiet moment. Asked by a user
/// (`by`), it goes as soon as nothing is mid-drag or mid-transition and its alerts show where
/// the click was; otherwise an automatic install, which also waits for the user to be away.
/// A click joins an install already under way.
fn begin_install(release: Release, by: Asker) {
    let asked = by.asked();
    let under_way = with(|s| match &mut s.install {
        Install::Idle => {
            s.install = Install::Preparing { version: release.version.clone(), by };
            None
        }
        Install::Preparing { version, by: b } => {
            *b = (*b).max(by);
            Some(version.to_string())
        }
        Install::Ready { prepared, by: b, .. } => {
            *b = (*b).max(by);
            Some(prepared.version.to_string())
        }
    });
    match under_way {
        Some(None) => {}
        Some(Some(version)) => {
            // A click joins it: it installs at the next quiet moment, not only when away.
            tracing::info!(asked, version, "update: an install is already under way");
            return;
        }
        None => return,
    }
    tracing::info!(version = %release.version, asked, "update: downloading and verifying");
    let r = release.clone();
    updater::prepare(release, move |result| later(move || prepared(r, result)));
}

/// The download and verification of `release` ended.
fn prepared(release: Release, result: Result<Prepared, PrepareError>) {
    let Some(Some((mtm, by))) = with(|s| {
        let by = match &s.install {
            Install::Preparing { by, .. } => *by,
            _ => return None,
        };
        s.install = Install::Idle;
        Some((s.mtm, by))
    }) else {
        return;
    };
    let asked = by.asked();
    let current = updates::current();
    match result {
        Ok(prepared) => {
            let tick = RcBlock::new(|_t: NonNull<NSTimer>| install_if_quiet());
            // SAFETY: the block runs on the main run loop, which keeps the timer; the install
            // state keeps it too and invalidates it.
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(QUIET_TICK, true, &tick) };
            with(|s| s.install = Install::Ready { prepared, by, timer, told: false });
            install_if_quiet();
        }
        Err(PrepareError::Manual(refusal, dmg)) => {
            tracing::info!(?refusal, "update: verified, but this copy cannot replace itself; opening the disk image");
            updater::open_disk_image(&dmg);
            present(
                mtm,
                by,
                AlertSpec {
                    key: "update",
                    title: format!("Install Stage Manager {} from the disk image", release.version),
                    text: refusal.sentence(),
                    buttons: vec!["OK".into()],
                    cancel: Some(0),
                    warning: false,
                    activate: asked,
                },
                |_| {},
            );
        }
        Err(PrepareError::Failed(why)) => {
            tracing::warn!(version = %release.version, "update: not installed: {why}");
            // No Skip This Version here (review 8C #1): a failure may be a network blip, and
            // it must not silence the release; the next daily check offers it again.
            present(mtm, by, install_failed_spec(&release, &why, &current, asked), move |b| {
                if b == 0 {
                    open_url(release.download_url());
                }
            });
        }
    }
}

/// The alert when a download or verification failed: Download or OK, never Skip This Version.
pub fn install_failed_spec(release: &Release, why: &str, current: &updates::Version, asked: bool) -> AlertSpec {
    AlertSpec {
        key: "update",
        title: format!("Stage Manager {} couldn't be installed", release.version),
        text: install_failed_text(why, current),
        buttons: vec!["Download".into(), "OK".into()],
        cancel: Some(1),
        warning: true,
        activate: asked,
    }
}

/// The text when a download or verification failed: why, and that nothing changed.
pub fn install_failed_text(why: &str, current: &updates::Version) -> String {
    let mut why = why.trim().trim_end_matches('.').to_string();
    if let Some(first) = why.get(..1) {
        why = first.to_uppercase() + &why[1..];
    }
    format!("{why}. Nothing was changed: you still have Stage Manager {current}. You can download the new version from GitHub instead.")
}

/// Seconds since the last keyboard, mouse or trackpad event in this login session.
fn idle_secs() -> f64 {
    // kCGAnyInputEventType is all ones.
    CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, CGEventType(u32::MAX))
}

/// A ready update installs when the moment allows ([`self_update::may_install_now`]): the
/// helper is armed and the app quits through its quit path (every window back first).
fn install_if_quiet() {
    let Some(Some((asked, hooks))) = with(|s| match &s.install {
        Install::Ready { by, .. } => {
            let asked = by.asked();
            if !asked && !(s.config.app.install_updates && s.config.app.check_for_updates) {
                // Automatic installs were turned off while it waited.
                if let Install::Ready { timer, prepared, .. } = std::mem::replace(&mut s.install, Install::Idle) {
                    timer.invalidate();
                    // Its staged copy goes too (review 8C #6).
                    updater::discard(&prepared);
                }
                tracing::info!("update: automatic install turned off; not installing");
                return None;
            }
            Some((asked, s.hooks))
        }
        _ => None,
    }) else {
        return;
    };
    let now = Instant::now();
    let locked = session::screen_locked();
    let asleep = CGDisplayIsAsleep(CGMainDisplayID());
    // Asked only for an automatic install: a click-asked one never waits for it.
    let kept_awake = !asked && session::display_kept_awake();
    if locked || asleep || kept_awake {
        session::saw_rest(now);
    }
    let idle = self_update::idle_after_rest(idle_secs(), session::since_rest(now).map(|d| d.as_secs_f64()));
    let moment = Moment { buttons_down: NSEvent::pressedMouseButtons() != 0, strip_busy: super::strip::busy(), locked, asleep, kept_awake, idle_secs: idle };
    if let Some(wait) = self_update::wait_reason(asked, moment) {
        if wait.is_rest() {
            with(|s| {
                if let Install::Ready { prepared, told, .. } = &mut s.install
                    && !std::mem::replace(told, true)
                {
                    tracing::info!(version = %prepared.version, ?moment, "update: waiting; {}: it installs {} s without input after that ends", wait.name(), self_update::AUTO_IDLE_SECS);
                }
            });
        }
        return;
    }
    let Some(prepared) = with(|s| match std::mem::replace(&mut s.install, Install::Idle) {
        Install::Ready { prepared, timer, .. } => {
            timer.invalidate();
            Some(prepared)
        }
        other => {
            s.install = other;
            None
        }
    })
    .flatten() else {
        return;
    };
    tracing::info!(version = %prepared.version, asked, ?moment, "update: installing; quitting first so every window comes back");
    updater::hand_off(&prepared, true);
    later(move || (hooks.quit)());
}

/// The app's exit path, after every window came back (main thread; elsewhere, nothing): a
/// ready update installs at the user's own Quit, without reopening the app (the owner's go 11-09),
/// and never at a quit that is not theirs ([`self_update::quit_installs`]). The helper
/// starts right after this, with the rest of the exit.
pub fn install_on_quit(user_quit: bool, clean: bool, for_logout: bool) {
    let Some(Some(facts)) = with(|s| {
        matches!(s.install, Install::Ready { .. }).then(|| QuitFacts { user_quit, clean, for_logout, apple_on: s.apple_on(), helper_armed: updater::armed() })
    }) else {
        return;
    };
    if !self_update::quit_installs(facts) {
        tracing::info!(?facts, "update: a ready update does not install at this quit; the next launch checks again");
        return;
    }
    let Some(prepared) = with(|s| match std::mem::replace(&mut s.install, Install::Idle) {
        Install::Ready { prepared, timer, .. } => {
            timer.invalidate();
            Some(prepared)
        }
        other => {
            s.install = other;
            None
        }
    })
    .flatten() else {
        return;
    };
    tracing::info!(version = %prepared.version, "update: installing at the user's Quit; the new version is not opened");
    updater::hand_off(&prepared, false);
}

/// The note an update that did not go in left behind, shown once.
fn show_update_note() {
    let Some(note) = updater::take_note() else { return };
    tracing::warn!("update: {note}");
    let Some(mtm) = MainThreadMarker::new() else { return };
    alert::show(
        mtm,
        AlertSpec {
            key: "update",
            title: "The update wasn't installed".into(),
            text: note,
            buttons: vec!["OK".into()],
            cancel: Some(0),
            warning: true,
            activate: false,
        },
        |_| {},
    );
}

/// Turns Apple's Stage Manager off (the user's click), then checks within about 2 s that it
/// stopped: `GloballyEnabled` reads back 0 and `WindowManager` shows no more windows than
/// before. Otherwise System Settings → Desktop & Dock opens with a one-line hint (a sheet on
/// Settings when `by` clicked there).
pub fn turn_off_apple(by: Asker) {
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
                present(
                    mtm,
                    by,
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

    /// Round settings-sheet: a click in Settings outranks a menu click, which outranks an
    /// automatic run, when one joins another; only a user's alerts take the focus.
    #[test]
    fn the_asker_who_clicked_last_or_in_settings_decides() {
        assert!(Asker::Nobody < Asker::User && Asker::User < Asker::Settings);
        assert_eq!(Asker::Nobody.max(Asker::Settings), Asker::Settings, "Settings joins an automatic check: its sheet");
        assert_eq!(Asker::Settings.max(Asker::User), Asker::Settings, "a menu click joins a Settings check: still its sheet");
        assert!(!Asker::Nobody.asked() && Asker::User.asked() && Asker::Settings.asked());
    }

    /// The menu bar's Check for Updates… and Turn Off: a sheet on Settings only while
    /// Settings is frontmost and key, else free-standing.
    #[test]
    fn a_menu_click_counts_as_settings_only_while_settings_is_in_front() {
        assert_eq!(Asker::from_menu(true), Asker::Settings);
        assert_eq!(Asker::from_menu(false), Asker::User);
        assert_eq!(alert::place(Asker::from_menu(false) == Asker::Settings, true, false), Place::Free);
    }

    /// The 1.7 review: a failed first registration is not marked done, so the next launch
    /// tries it again; launch at login off is done without asking macOS.
    #[test]
    fn a_failed_first_registration_is_tried_again() {
        assert!(first_login_set_up(true, || Ok(())));
        assert!(!first_login_set_up(true, || Err("macOS said no".into())));
        assert!(first_login_set_up(false, || panic!("not asked")));
    }

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

    /// Review R2: a config read that fails keeps the last good config on both sides; only a
    /// readable empty file means the defaults.
    #[test]
    fn an_unreadable_config_keeps_the_last_good_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("config.toml");
        std::fs::write(&file, "[app]\nlaunch_at_login = false\ncheck_for_updates = false\nshow_menu_bar_icon = false\n").expect("write");
        let (mut store, opened) = config::ConfigStore::open(file.clone());
        assert!(opened.is_ok());
        let good = store.config().clone();
        assert!(!good.app.launch_at_login && !good.app.check_for_updates);

        let read = || std::fs::read_to_string(&file);
        std::fs::write(&file, [0xff]).expect("write");
        assert!(store.reload().is_err(), "not UTF-8");
        assert_eq!(parse_read(read()), None, "the shell's own read gives nothing either");
        std::fs::remove_file(&file).expect("remove");
        assert!(store.reload().is_err(), "missing");
        assert_eq!(parse_read(read()), None);
        std::fs::create_dir(&file).expect("mkdir");
        assert!(store.reload().is_err(), "a folder");
        assert_eq!(parse_read(read()), None);
        // What the WM sends after each failure is the last good config: nothing changes.
        assert_eq!(store.config(), &good);
        assert_eq!(app_effects(&good.app, &store.config().app), AppEffects { menu_icon: None, login_item: None });

        std::fs::remove_dir(&file).expect("rmdir");
        std::fs::write(&file, "").expect("write");
        assert!(store.reload().is_ok(), "an empty file is valid");
        assert_eq!(parse_read(read()), Some(Config::default()));
        assert_eq!(store.config(), &Config::default());
        let defaults = Config::default().app;
        assert_eq!(
            app_effects(&good.app, &store.config().app),
            AppEffects {
                menu_icon: Some(defaults.show_menu_bar_icon),
                login_item: Some(defaults.launch_at_login)
            }
        );
    }

    /// Round reopen-list: the way launch at login was last set to, and its move.
    #[test]
    fn the_login_way_moves_once_whichever_way_the_key_says() {
        let fresh = AppState::default();
        assert_eq!(login_way_done(&fresh), Service::MainApp, "1.6.0 and earlier: the login item");
        let early = AppState { login_agent_moved: true, ..Default::default() };
        assert_eq!(login_way_done(&early), Service::Agent, "round early-start's build moved it to the agent");
        let back = AppState { login_agent_moved: true, login_start: Some("login_item".into()), ..Default::default() };
        assert_eq!(login_way_done(&back), Service::MainApp, "moved back: the key's value is remembered");
        let defaults = config::AppConfig::default();
        assert_eq!(login_way(&defaults), Service::MainApp, "the default");
        let agent = config::AppConfig { login_start: LoginStart::BackgroundItem, ..defaults };
        assert_eq!(login_way(&agent), Service::Agent);
        assert_eq!(app_effects(&defaults, &agent), AppEffects { menu_icon: None, login_item: Some(true) }, "the key changed while on: moved");
        assert_eq!(app_effects(&agent, &defaults).login_item, Some(true), "and back");
        let off = config::AppConfig { launch_at_login: false, ..defaults };
        let off_agent = config::AppConfig { launch_at_login: false, ..agent };
        assert_eq!(app_effects(&off, &off_agent).login_item, None, "off: nothing to move");
    }

    #[test]
    fn a_launch_after_a_listed_logout_is_told_apart() {
        let s = Duration::from_secs;
        let now = 1_000_000;
        assert!(listed_before_login(Some(now - 100), now, Some(s(5))), "listed at the logout, 95 s before this login");
        assert!(!listed_before_login(Some(now - 2), now, Some(s(5))), "listed after the login: not this login's");
        assert!(!listed_before_login(None, now, Some(s(5))));
        assert!(!listed_before_login(Some(now - 100), now, None), "no login time");
        // Round logout-cancel-relaunch: the guardian opens the app again 40 s after a logout
        // cancelled once macOS had quit it, in the login of hours ago: no reopen of a login.
        assert!(!listed_before_login(Some(now - 40), now, Some(s(3 * 3600))), "a relaunch after a cancelled logout");
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
        let old = updates::Version::parse("1.0.0").expect("version");
        let o = offer_spec(&r, &old, &Ok(PathBuf::from("/Applications/Stage Manager.app")));
        assert_eq!(o.buttons, vec!["Install and Relaunch", "Later", "Skip This Version"], "the owner's words, 10-13");
        assert_eq!(o.cancel, Some(1), "Escape is Later");
        assert!(o.text.contains("Stage Manager 1.1.0 is available — you have 1.0.0"));
        let local = offer_spec(&r, &old, &Err(Refusal::NotDeveloperId));
        assert_eq!(local.buttons, vec!["Download", "Later", "Skip This Version"], "a copy that cannot replace itself downloads");
        assert!(local.text.contains("can't install updates itself"));
        let failed = install_failed_text("its code signature is not valid or not ours: x.", &old);
        assert!(failed.starts_with("Its code signature"));
        assert!(failed.contains("Nothing was changed: you still have Stage Manager 1.0.0."));
        // Review 8C #1: a failed install (maybe a network blip) never offers to skip the release.
        for asked in [true, false] {
            let f = install_failed_spec(&r, "the download did not finish", &old, asked);
            assert_eq!(f.buttons, vec!["Download", "OK"], "asked: {asked}");
            assert_eq!(f.cancel, Some(1));
            assert_eq!(f.activate, asked);
        }
        assert!(o.activate, "the spec activates; an automatic offer turns it off");
        assert_eq!(turn_off_wait(), Duration::from_secs(2));
    }
}
