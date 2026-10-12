//! `stage-manager`: the app, its `ctl` client, its guardian and a window-list probe.
//!
//! - `stage-manager` runs the menu bar app.
//! - `stage-manager ctl <words...>` sends one command to the running app's ctl socket.
//! - `stage-manager --guardian <pid>` is the guardian process the app spawns at launch.
//! - `stage-manager config --print-defaults | --explain <section.key> | --reference |
//!   --check [file]` prints the commented default config, explains a key, prints README's
//!   configuration table, or validates a file.
//! - `stage-manager probe-windows <bundle-id>` prints that app's windows as the window
//!   server reports them (bounds, layer, onscreen, z), one JSON line, for test scripts.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        #[cfg(target_os = "macos")]
        Some("ctl") => stage_manager::platform::ctl::run_client(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("strip-demo") => stage_manager::ui::strip_demo::run(),
        Some("config") => {
            let (code, out) = stage_manager::config::command(&args[2..]);
            if code == 0 { print!("{out}") } else { eprint!("{out}") }
            ExitCode::from(code as u8)
        }
        #[cfg(target_os = "macos")]
        Some("--guardian") => stage_manager::platform::safety::run_guardian(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-windows") => app::probe_windows(&args[2..]),
        Some("--version") => {
            println!("stage-manager {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "macos")]
        None => app::run(),
        _ => {
            eprintln!(
                "usage: stage-manager [ctl <command...> | config <--print-defaults | --explain KEY | --reference | --check [FILE]> | strip-demo | probe-windows <bundle-id> | --version]"
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(target_os = "macos")]
mod app {
    //! App bootstrap: NSApplication (accessory), status item, permission, settings and
    //! display polling, WM thread, workspace observers, hotkeys, the wallpaper-click monitor,
    //! safety layers and the ctl socket.

    use std::cell::RefCell;
    use std::process::ExitCode;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
    use objc2_foundation::{MainThreadMarker, NSString, NSTimer, NSURL};
    use serde_json::json;

    use stage_manager::config::{Config, HotkeyConfig, default_file};
    use stage_manager::engine::model::Display;
    use stage_manager::platform::apps::{self, WorkspaceObservers};
    use stage_manager::platform::controller::{self, UiUpdate};
    use stage_manager::platform::ctl::{self, Command};
    use stage_manager::platform::desktop;
    use stage_manager::platform::input::{self, InputKind, InputMonitor};
    use stage_manager::platform::native_settings::{self, RawSettings};
    use stage_manager::platform::permissions::{self, PermissionState, PrivacyPane};
    use stage_manager::platform::wm_thread::{self, WmCommand, WmHandle};
    use stage_manager::platform::{hotkeys, logging, paths, safety, windows};
    use stage_manager::ui::status_menu::{self, MenuAction, MenuState, StatusMenu};
    use stage_manager::ui::{strip, strip_model};

    /// Permission, settings and display poll period.
    const POLL_SECONDS: f64 = 2.0;

    /// How long Quit waits for the WM thread to put `windows` windows back: the action in
    /// flight when Quit came (a batch stops between actions), then a frame per window, each
    /// up to three AX calls (size, position, size) that a slow or hung app may let run to
    /// the 0.25 s messaging timeout. At least 4 s, at most 30 s.
    pub(crate) fn shutdown_timeout(windows: usize) -> Duration {
        let per_window = 3.0 * f64::from(wm_thread::AX_TIMEOUT_SECONDS) + 0.05;
        let secs = 2.0 + per_window * windows as f64;
        Duration::from_secs_f64(secs.clamp(4.0, 30.0))
    }

    /// After a signal, the main thread gets the shutdown timeout plus this before the
    /// watcher thread unparks on its own.
    const SIGNAL_GRACE_EXTRA: Duration = Duration::from_secs(2);

    /// The WM thread's handle, for Quit: reachable even while the app state is borrowed
    /// (a menu action runs inside that borrow).
    static WM: OnceLock<WmHandle> = OnceLock::new();

    /// `STAGE_MANAGER_TEST=1` enables test-only ctl commands (`debug-panic`).
    const TEST_ENV: &str = "STAGE_MANAGER_TEST";

    static QUITTING: AtomicBool = AtomicBool::new(false);

    struct App {
        menu: StatusMenu,
        wm: WmHandle,
        permissions: PermissionState,
        settings: RawSettings,
        strip_visible: bool,
        config_error: Option<String>,
        hotkeys: Option<HotkeyConfig>,
        thumbnail_interval_ms: Option<u64>,
        display: Option<Display>,
        _workspace: WorkspaceObservers,
        _input: Option<InputMonitor>,
        _timer: Retained<NSTimer>,
    }

    thread_local! {
        static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    }

    fn with_app(f: impl FnOnce(&mut App)) {
        APP.with(|a| {
            if let Ok(mut guard) = a.try_borrow_mut()
                && let Some(app) = guard.as_mut()
            {
                f(app);
            }
        });
    }

    impl App {
        fn menu_state(&self) -> MenuState {
            MenuState {
                permissions: self.permissions,
                apple_stage_manager_on: native_settings::apple_stage_manager_enabled(&self.settings),
                strip_visible: self.strip_visible,
                config_error: self.config_error.clone(),
            }
        }

        fn refresh_menu(&self) {
            self.menu.update(&self.menu_state());
        }

        /// Runs every 2 s: permissions, `com.apple.WindowManager`, the main display.
        fn poll(&mut self) {
            let now = PermissionState::current();
            if now != self.permissions {
                tracing::info!(
                    accessibility = now.accessibility,
                    screen_recording = now.screen_recording,
                    "permissions changed"
                );
                if now.accessibility != self.permissions.accessibility {
                    self.wm.send(WmCommand::Accessibility(now.accessibility));
                }
                self.permissions = now;
                self.refresh_menu();
            }
            if let Some(settings) = native_settings::refresh() {
                warn_if_apple_stage_manager(&settings);
                self.settings = settings.clone();
                self.wm.send(WmCommand::NativeSettings(settings));
                self.refresh_menu();
            }
            self.send_display_if_changed();
        }

        fn send_display_if_changed(&mut self) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let Some(main) = desktop::main_display(mtm) else { return };
            if self.display != Some(main) {
                tracing::info!(frame = %main.frame, visible = %main.visible, "main display");
                self.display = Some(main);
                self.wm.send(WmCommand::Displays(vec![main]));
            }
        }

        fn on_ui(&mut self, update: UiUpdate) {
            match update {
                UiUpdate::Config { error, hotkeys, thumbnail_interval_ms } => {
                    self.config_error = error;
                    if self.thumbnail_interval_ms != Some(thumbnail_interval_ms) {
                        strip::set_thumbnail_interval(thumbnail_interval_ms);
                        self.thumbnail_interval_ms = Some(thumbnail_interval_ms);
                    }
                    if self.hotkeys.as_ref() != Some(&hotkeys) {
                        let r = hotkeys::register(&hotkeys.bindings());
                        tracing::info!(active = r.active, failed = ?r.failed, "hotkeys registered");
                        self.hotkeys = Some(hotkeys);
                    }
                }
                UiUpdate::StripVisible(v) => self.strip_visible = v,
            }
            self.refresh_menu();
        }

        fn on_menu(&mut self, action: MenuAction) {
            match action {
                MenuAction::GrantAccessibility => {
                    // A user click: let macOS list the app in the pane, then open it.
                    permissions::check_accessibility(true);
                    open_url(PrivacyPane::Accessibility.url());
                }
                MenuAction::GrantScreenRecording => {
                    permissions::request_screen_recording();
                    open_url(PrivacyPane::ScreenRecording.url());
                }
                MenuAction::ToggleStrip => {
                    self.wm.send(WmCommand::ToggleStrip);
                }
                MenuAction::ReloadConfig => {
                    self.wm.send(WmCommand::ReloadConfig);
                }
                MenuAction::OpenConfig => open_config(),
                MenuAction::Quit => quit(),
            }
        }
    }

    fn open_config() {
        let path = paths::config_file();
        if !path.exists() {
            let written = std::fs::create_dir_all(paths::config_dir()).and_then(|_| std::fs::write(&path, default_file()));
            if let Err(e) = written {
                tracing::warn!("cannot create {}: {e}", path.display());
                return;
            }
        }
        if let Err(e) = std::process::Command::new("/usr/bin/open").arg("-t").arg(&path).status() {
            tracing::warn!("cannot open {}: {e}", path.display());
        }
    }

    fn warn_if_apple_stage_manager(settings: &RawSettings) {
        if native_settings::apple_stage_manager_enabled(settings) {
            tracing::warn!("Apple's Stage Manager is on (GloballyEnabled = 1); turn it off while Stage Manager runs");
        }
    }

    fn open_url(url: &str) {
        match NSURL::URLWithString(&NSString::from_str(url)) {
            Some(url) => {
                if !NSWorkspace::sharedWorkspace().openURL(&url) {
                    tracing::warn!("could not open {url:?}");
                }
            }
            None => tracing::warn!("bad URL {url}"),
        }
    }

    /// Quits cleanly (main thread): every parked window back onscreen through the WM thread
    /// (the registry and a recovery scan as fallbacks), the clean-exit marker for the
    /// guardian, the socket removed, then exit.
    ///
    /// The WM thread is told first ([`safety::request_quit`]) so a long batch of window moves
    /// stops between two actions and Quit is answered next.
    fn quit() {
        if QUITTING.swap(true, Ordering::SeqCst) {
            return;
        }
        safety::request_quit();
        let timeout = shutdown_timeout(safety::registry_len());
        tracing::info!(timeout_ms = timeout.as_millis() as u64, "quitting: unparking every window");
        let ok = match WM.get().map(|wm| wm.shutdown(timeout)) {
            Some(Some(true)) => true,
            Some(Some(false)) => {
                tracing::warn!("some windows did not come back through the WM thread; retrying from the registry");
                safety::emergency_unpark("quit")
            }
            Some(None) => {
                tracing::warn!("WM thread did not answer; unparking from the registry");
                safety::emergency_unpark("quit")
            }
            None => {
                tracing::warn!("no WM thread; unparking from the registry");
                safety::emergency_unpark("quit")
            }
        };
        finish_exit(ok, 0);
    }

    fn finish_exit(ok: bool, code: i32) -> ! {
        if ok {
            safety::mark_clean_exit();
        } else {
            tracing::error!("not every window could be restored; the guardian will scan after exit");
        }
        ctl::remove_socket();
        tracing::info!("bye");
        std::process::exit(code);
    }

    fn ctl_handler(wm: WmHandle) -> ctl::Handler {
        Arc::new(move |cmd| match cmd {
            Command::Ping => json!({
                "ok": true,
                "pong": true,
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
            }),
            Command::Quit => {
                DispatchQueue::main().exec_async(quit);
                json!({ "ok": true, "quitting": true })
            }
            Command::DebugMenuQuit => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-menu-quit needs STAGE_MANAGER_TEST=1");
                }
                // The menu's path: the action runs while the app state is borrowed.
                DispatchQueue::main().exec_async(|| with_app(|app| app.on_menu(MenuAction::Quit)));
                json!({ "ok": true, "quitting": true })
            }
            Command::DebugPanic => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-panic needs STAGE_MANAGER_TEST=1");
                }
                tracing::warn!("debug-panic: panicking on the main thread");
                DispatchQueue::main().exec_async(|| panic!("debug-panic requested over ctl"));
                json!({ "ok": true, "panicking": true })
            }
            Command::DebugClick { x, y, shift } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-click needs STAGE_MANAGER_TEST=1");
                }
                match input::click(stage_manager::engine::geometry::Point::new(x, y), shift) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => ctl::error_reply(&e),
                }
            }
            Command::DebugDrag { points, shift } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-drag needs STAGE_MANAGER_TEST=1");
                }
                match input::drag(&points, shift, Duration::from_millis(400)) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => ctl::error_reply(&e),
                }
            }
            other => wm
                .ctl(other, Duration::from_secs(4))
                .unwrap_or_else(|| ctl::error_reply("WM thread did not answer")),
        })
    }

    fn on_signal(_sig: i32) {
        let grace = shutdown_timeout(safety::registry_len()) + SIGNAL_GRACE_EXTRA;
        DispatchQueue::main().exec_async(quit);
        std::thread::sleep(grace);
        tracing::error!("main thread did not quit after a signal; unparking from the signal watcher");
        QUITTING.store(true, Ordering::SeqCst);
        let ok = safety::emergency_unpark("signal");
        finish_exit(ok, 1);
    }

    /// Runs the menu bar app until Quit.
    pub fn run() -> ExitCode {
        logging::init();
        safety::install_panic_hook();
        tracing::info!(version = env!("CARGO_PKG_VERSION"), pid = std::process::id(), "Stage Manager starting");
        if let Some(home) = paths::custom_home() {
            tracing::info!("STAGE_MANAGER_HOME={}", home.display());
        }
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::error!("not on the main thread");
            return ExitCode::FAILURE;
        };

        if ctl::request("ping", Duration::from_millis(500)).is_ok() {
            tracing::error!("another Stage Manager is already running; exiting");
            return ExitCode::FAILURE;
        }

        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        controller::set_ui_sink(|update| {
            DispatchQueue::main().exec_async(move || with_app(|app| app.on_ui(update)));
        });
        let wm = match WmHandle::spawn() {
            Ok(wm) => wm,
            Err(e) => {
                tracing::error!("cannot start the WM thread: {e}");
                return ExitCode::FAILURE;
            }
        };
        let _ = WM.set(wm.clone());
        safety::spawn_guardian();
        let wm_signals = wm.clone();
        if let Err(e) = safety::watch_signals(move |sig| {
            let _ = &wm_signals;
            on_signal(sig);
        }) {
            tracing::error!("cannot watch signals: {e}");
        }

        let wm_strip = wm.clone();
        strip_model::set_event_sink(move |event| {
            wm_strip.send(WmCommand::Strip(event));
        });
        let wm_keys = wm.clone();
        hotkeys::install(move |command| {
            wm_keys.send(WmCommand::Hotkey(command));
        });

        wm.send(WmCommand::Apps(apps::running_regular_apps()));
        let wm_events = wm.clone();
        let workspace = apps::observe_workspace(move |event| {
            wm_events.send(WmCommand::App(event));
        });

        let permissions = permissions::check_at_launch();
        tracing::info!(
            accessibility = permissions.accessibility,
            screen_recording = permissions.screen_recording,
            prompts = permissions::prompts_allowed(),
            "permissions at launch"
        );
        let settings = native_settings::refresh().unwrap_or_else(native_settings::current);
        warn_if_apple_stage_manager(&settings);
        wm.send(WmCommand::NativeSettings(settings.clone()));

        let wm_input = wm.clone();
        let input_monitor = input::monitor(mtm, move |event| {
            let over_strip = event.kind == InputKind::MouseUp && strip::is_drop_target(event.at);
            wm_input.send(WmCommand::Input(event, over_strip));
        });
        if input_monitor.is_none() {
            tracing::warn!("cannot monitor input; wallpaper clicks, window drops on the strip and Shift sugar are off");
        }

        if let Err(e) = ctl::serve(ctl_handler(wm.clone())) {
            tracing::error!("ctl socket unavailable: {e}");
        }

        let tick = RcBlock::new(|_timer: NonNull<NSTimer>| with_app(App::poll));
        // SAFETY: the block runs on the main run loop only.
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(POLL_SECONDS, true, &tick) };

        let menu = StatusMenu::new(mtm);
        let mut state = App {
            menu,
            wm: wm.clone(),
            permissions,
            settings,
            strip_visible: true,
            config_error: None,
            hotkeys: None,
            thumbnail_interval_ms: None,
            display: None,
            _workspace: workspace,
            _input: input_monitor,
            _timer: timer,
        };
        state.send_display_if_changed();
        state.refresh_menu();
        APP.with(|a| *a.borrow_mut() = Some(state));
        // The loaded config's interval follows in the first UiUpdate::Config.
        strip::install(mtm, Config::default().strip.thumbnail_interval_ms);
        status_menu::set_action_handler(|action| with_app(|app| app.on_menu(action)));

        if permissions.accessibility {
            wm.send(WmCommand::Accessibility(true));
        } else {
            tracing::warn!("Accessibility not granted; observation starts once it is");
        }

        app.run();
        ExitCode::SUCCESS
    }

    /// `probe-windows <bundle-id>`: the window server's view of one app's windows.
    pub fn probe_windows(args: &[String]) -> ExitCode {
        let Some(bundle) = args.first() else {
            eprintln!("usage: stage-manager probe-windows <bundle-id>");
            return ExitCode::from(2);
        };
        let pids: Vec<i32> = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle))
            .iter()
            .map(|a| a.processIdentifier())
            .collect();
        let list = windows::list_windows();
        let z = windows::z_order(&list);
        let out: Vec<_> = list
            .iter()
            .filter(|w| pids.contains(&w.pid))
            .map(|w| {
                json!({
                    "id": w.id,
                    "pid": w.pid,
                    "layer": w.layer,
                    "onscreen": w.onscreen,
                    "bounds": w.bounds,
                    "z": z.get(&w.id),
                })
            })
            .collect();
        let display = desktop::cg_display_bounds().first().copied();
        println!("{}", json!({ "ok": true, "pids": pids, "display": display, "windows": out }));
        ExitCode::SUCCESS
    }
}
