//! `stage-manager`: the app, its `ctl` client, its guardian and a window-list probe.
//!
//! - `stage-manager` runs the menu bar app.
//! - `stage-manager ctl <words...>` sends one command to the running app's ctl socket.
//! - `stage-manager --guardian <pid>` is the guardian process the app spawns at launch.
//! - `stage-manager config --print-defaults | --explain <section.key> | --reference |
//!   --check [file]` prints the commented default config, explains a key, prints README's
//!   configuration table, or validates a file.
//! - `stage-manager probe-windows <bundle-id>...` prints those apps' windows as the window
//!   server reports them (bounds, layer, onscreen, z), one JSON line, for test scripts.
//! - `stage-manager probe-screen` prints the on-screen windows that cover the main display
//!   (a full-screen app's Space), one JSON line, for test scripts.
//! - `stage-manager debug-post-space left|right [numpad]` (test only, `STAGE_MANAGER_TEST=1`,
//!   launched through LaunchServices so it has the app's Accessibility) types the Desktop
//!   switch shortcut once and exits: the Spaces check's way to switch while the app is down.

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
        Some("--guardian") => stage_manager::platform::guardian::run(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-windows") => app::probe_windows(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-screen") => app::probe_screen(),
        #[cfg(target_os = "macos")]
        Some("debug-post-space") => app::post_space(&args[2..]),
        Some("--version") => {
            println!("stage-manager {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "macos")]
        None => app::run(),
        _ => {
            eprintln!(
                "usage: stage-manager [ctl <command...> | config <--print-defaults | --explain KEY | --reference | --check [FILE]> | strip-demo | probe-windows <bundle-id>... | probe-screen | --version]"
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
    use std::time::{Duration, Instant};

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
    use stage_manager::engine::fullscreen;
    use stage_manager::platform::space_keys::{self, Direction, Expose, SpaceKey, SpaceView};
    use stage_manager::platform::{guardian, hotkeys, logging, paths, safety, windows};
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
        _spaces: WorkspaceObservers,
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
            Command::DebugHover { x, y, ms } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-hover needs STAGE_MANAGER_TEST=1");
                }
                match input::hover(stage_manager::engine::geometry::Point::new(x, y), Duration::from_millis(ms.min(5000))) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => ctl::error_reply(&e),
                }
            }
            Command::DebugSpace { direction, numpad } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-space needs STAGE_MANAGER_TEST=1");
                }
                debug_space(&wm, direction, numpad)
            }
            Command::DebugExpose { which } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-expose needs STAGE_MANAGER_TEST=1");
                }
                debug_expose(which)
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

    /// `ctl debug-expose`: types Mission Control's own shortcut as the user's settings have
    /// it (or Escape) at the HID tap. The test watches the window server for the outcome.
    fn debug_expose(which: Option<Expose>) -> serde_json::Value {
        let Some(which) = which else {
            return match space_keys::post_plain(space_keys::ESCAPE_KEY) {
                Ok(()) => {
                    tracing::info!("debug-expose: Escape typed");
                    json!({ "ok": true, "key": "escape" })
                }
                Err(e) => ctl::error_reply(&e),
            };
        };
        let key = SpaceKey::read_expose(which);
        if let Err(e) = space_keys::post(&key, false) {
            return ctl::error_reply(&e);
        }
        tracing::info!(which = which.name(), key = key.describe(), "debug-expose: shortcut typed");
        json!({ "ok": true, "which": which.name(), "key": key.describe(), "key_from_settings": key.from_settings })
    }

    /// The Space shown as the engine sees it.
    fn space_view(wm: &WmHandle) -> Option<SpaceView> {
        wm.ctl(Command::Space, Duration::from_secs(2)).and_then(|v| SpaceView::from_json(&v))
    }

    /// `ctl debug-space`: types the Desktop switch shortcut at the HID tap, then waits up to
    /// [`ctl::SPACE_WAIT`] for the engine to handle the Space change. Runs on a ctl
    /// connection thread (never the main thread: the change notification comes there).
    fn debug_space(wm: &WmHandle, direction: Direction, numpad: bool) -> serde_json::Value {
        let Some(before) = space_view(wm) else {
            return ctl::error_reply("the engine did not say which Space shows");
        };
        if before.settling {
            return ctl::error_reply("a Space change is still settling");
        }
        let key = SpaceKey::read(direction);
        if let Err(e) = space_keys::post(&key, numpad) {
            return ctl::error_reply(&e);
        }
        tracing::info!(direction = direction.name(), key = key.describe(), numpad, "debug-space: shortcut typed");
        let start = Instant::now();
        let mut last = before;
        loop {
            std::thread::sleep(Duration::from_millis(150));
            if let Some(now) = space_view(wm) {
                last = now;
                if now.moved_from(&before) {
                    break;
                }
            }
            if start.elapsed() >= ctl::SPACE_WAIT {
                break;
            }
        }
        let changed = last.moved_from(&before);
        tracing::info!(changed, from = before.space, to = last.space, fullscreen = last.fullscreen, "debug-space");
        json!({
            "ok": true,
            "changed": changed,
            "before": before,
            "after": last,
            "ms": start.elapsed().as_millis() as u64,
            "key": key.describe(),
            "key_from_settings": key.from_settings,
            "numpad": numpad,
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
        guardian::spawn();
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
        let wm_spaces = wm.clone();
        let spaces = apps::observe_space_changes(move || {
            strip::space_changed();
            wm_spaces.send(WmCommand::SpaceChanged);
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
            if event.kind == InputKind::KeyDown && event.cmd {
                // ⌘Tab may switch stages: fresh snapshots for the animation.
                strip::prefetch();
            }
            let drop = if event.kind == InputKind::MouseUp { strip::drop_slot(event.at) } else { None };
            wm_input.send(WmCommand::Input(event, drop));
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
            _spaces: spaces,
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

    /// `probe-windows <bundle-id>...`: the window server's view of those apps' windows.
    pub fn probe_windows(args: &[String]) -> ExitCode {
        if args.is_empty() {
            eprintln!("usage: stage-manager probe-windows <bundle-id>...");
            return ExitCode::from(2);
        }
        let pids: Vec<i32> = args
            .iter()
            .flat_map(|bundle| NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle)))
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
                    "alpha": w.alpha,
                    "bounds": w.bounds,
                    "z": z.get(&w.id),
                })
            })
            .collect();
        let display = desktop::cg_display_bounds().first().copied();
        println!("{}", json!({ "ok": true, "pids": pids, "display": display, "windows": out }));
        ExitCode::SUCCESS
    }

    /// `probe-screen`: the on-screen windows covering the whole main display (menu bar
    /// included), as a full-screen app's Space shows one (`covering`: layer 0; pids and
    /// bounds only), and those above or below the windows' layer (`overlays`: the owner's
    /// process name, never a window title; Mission Control and App Exposé show one of the
    /// Dock's, the Mission Control check's sign that they are up).
    pub fn probe_screen() -> ExitCode {
        let display = desktop::cg_display_bounds().first().copied();
        let list = windows::list_windows();
        let over = |layer0: bool| -> Vec<serde_json::Value> {
            display
                .map(|d| {
                    list.iter()
                        .filter(|w| w.onscreen && (w.layer == 0) == layer0 && fullscreen::covers_display(w.bounds, d))
                        .map(|w| {
                            if layer0 {
                                json!({ "id": w.id, "pid": w.pid, "bounds": w.bounds })
                            } else {
                                json!({ "id": w.id, "pid": w.pid, "owner": w.owner, "layer": w.layer, "alpha": w.alpha })
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let out = json!({ "ok": display.is_some(), "display": display, "covering": over(true), "overlays": over(false) });
        println!("{out}");
        ExitCode::SUCCESS
    }

    /// `debug-post-space left|right [numpad]` (test only): types the Desktop switch shortcut
    /// once and exits. Launched through LaunchServices (`open -n`) it has the app's
    /// Accessibility, so a test can switch Desktops while the app itself is down (the
    /// guardian drill). It never serves the ctl socket, so a guardian does not take it for
    /// Stage Manager running again. The outcome is appended to `post-space.log` in the log
    /// directory.
    pub fn post_space(args: &[String]) -> ExitCode {
        if std::env::var_os(TEST_ENV).is_none() {
            eprintln!("debug-post-space needs STAGE_MANAGER_TEST=1");
            return ExitCode::from(2);
        }
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let (direction, numpad) = match words.as_slice() {
            [d] => (Direction::parse(d), false),
            [d, "numpad"] => (Direction::parse(d), true),
            _ => (None, false),
        };
        let Some(direction) = direction else {
            eprintln!("usage: stage-manager debug-post-space left|right [numpad]");
            return ExitCode::from(2);
        };
        let key = SpaceKey::read(direction);
        let result = space_keys::post(&key, numpad);
        let line = json!({
            "pid": std::process::id(),
            "direction": direction.name(),
            "key": key.describe(),
            "numpad": numpad,
            "accessibility": permissions::accessibility_trusted(),
            "ok": result.is_ok(),
            "error": result.as_ref().err(),
        });
        println!("{line}");
        let log = paths::log_dir().join("post-space.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            use std::io::Write as _;
            let _ = writeln!(f, "{line}");
        }
        if result.is_ok() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    }
}
