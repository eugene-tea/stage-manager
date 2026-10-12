//! `stage-manager`: the app, and its `ctl` client.
//!
//! - `stage-manager` runs the menu bar app.
//! - `stage-manager ctl <words...>` sends one command to the running app's ctl socket.
//! - `stage-manager --guardian <pid>` is reserved for the guardian process (milestone 3).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        #[cfg(target_os = "macos")]
        Some("ctl") => stage_manager::platform::ctl::run_client(&args[2..]),
        Some("--guardian") => {
            eprintln!("stage-manager: --guardian is reserved for a later milestone");
            ExitCode::from(2)
        }
        Some("--version") => {
            println!("stage-manager {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "macos")]
        None => app::run(),
        _ => {
            eprintln!("usage: stage-manager [ctl <command...> | --version]");
            ExitCode::from(2)
        }
    }
}

#[cfg(target_os = "macos")]
mod app {
    //! App bootstrap: NSApplication (accessory), status item, permission and settings
    //! polling, WM thread, workspace observers and the ctl socket.

    use std::cell::RefCell;
    use std::process::ExitCode;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::time::Duration;

    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWorkspace};
    use objc2_foundation::{MainThreadMarker, NSString, NSTimer, NSURL};
    use serde_json::json;

    use stage_manager::platform::apps::{self, WorkspaceObservers};
    use stage_manager::platform::ctl::{self, Command};
    use stage_manager::platform::native_settings::{self, RawSettings};
    use stage_manager::platform::permissions::{self, PermissionState, PrivacyPane};
    use stage_manager::platform::wm_thread::{WmCommand, WmHandle};
    use stage_manager::platform::{logging, paths};
    use stage_manager::ui::status_menu::{self, MenuAction, MenuState, StatusMenu};

    /// Permission and settings poll period.
    const POLL_SECONDS: f64 = 2.0;

    struct App {
        menu: StatusMenu,
        wm: WmHandle,
        permissions: PermissionState,
        settings: RawSettings,
        strip_visible: bool,
        _workspace: WorkspaceObservers,
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
            }
        }

        fn refresh_menu(&self) {
            self.menu.update(&self.menu_state());
        }

        /// Runs every 2 s: permissions and `com.apple.WindowManager`.
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
                    self.strip_visible = !self.strip_visible;
                    tracing::info!(visible = self.strip_visible, "strip toggled (the strip arrives in a later milestone)");
                    self.refresh_menu();
                }
                MenuAction::ReloadConfig => {
                    // INTEGRATOR: call the config module's reload here.
                    tracing::info!("reload config requested (config arrives with the engine)");
                }
                MenuAction::OpenConfig => match paths::ensure_config_file() {
                    Ok(path) => {
                        let opened = std::process::Command::new("/usr/bin/open").arg("-t").arg(&path).status();
                        if let Err(e) = opened {
                            tracing::warn!("cannot open {}: {e}", path.display());
                        }
                    }
                    Err(e) => tracing::warn!("cannot create the config file: {e}"),
                },
                MenuAction::Quit => quit(),
            }
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

    /// Quits cleanly (main thread): stops the WM thread, removes the socket, terminates.
    fn quit() {
        tracing::info!("quitting");
        let wm = APP.with(|a| a.try_borrow().ok().and_then(|g| g.as_ref().map(|app| app.wm.clone())));
        if let Some(wm) = wm
            && !wm.shutdown(Duration::from_secs(1))
        {
            tracing::warn!("WM thread did not confirm shutdown");
        }
        ctl::remove_socket();
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).terminate(None);
        }
        // terminate: returns only when cancelled; an accessory app has nothing to cancel.
        std::process::exit(0);
    }

    fn ctl_handler(wm: WmHandle) -> ctl::Handler {
        Arc::new(move |cmd| match cmd {
            Command::Ping => json!({
                "ok": true,
                "pong": true,
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
            }),
            Command::Dump => wm
                .dump(Duration::from_secs(3))
                .unwrap_or_else(|| ctl::error_reply("WM thread did not answer")),
            Command::Quit => {
                DispatchQueue::main().exec_async(quit);
                json!({ "ok": true, "quitting": true })
            }
        })
    }

    /// Runs the menu bar app until Quit.
    pub fn run() -> ExitCode {
        logging::init();
        tracing::info!(version = env!("CARGO_PKG_VERSION"), pid = std::process::id(), "Stage Manager starting");
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

        let wm = match WmHandle::spawn() {
            Ok(wm) => wm,
            Err(e) => {
                tracing::error!("cannot start the WM thread: {e}");
                return ExitCode::FAILURE;
            }
        };
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

        if let Err(e) = ctl::serve(ctl_handler(wm.clone())) {
            tracing::error!("ctl socket unavailable: {e}");
        }

        let tick = RcBlock::new(|_timer: NonNull<NSTimer>| with_app(App::poll));
        // SAFETY: the block runs on the main run loop only.
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(POLL_SECONDS, true, &tick) };

        let menu = StatusMenu::new(mtm);
        let state = App {
            menu,
            wm: wm.clone(),
            permissions,
            settings,
            strip_visible: true,
            _workspace: workspace,
            _timer: timer,
        };
        state.refresh_menu();
        APP.with(|a| *a.borrow_mut() = Some(state));
        status_menu::set_action_handler(|action| with_app(|app| app.on_menu(action)));

        if permissions.accessibility {
            wm.send(WmCommand::Accessibility(true));
        } else {
            tracing::warn!("Accessibility not granted; observation starts once it is");
        }

        app.run();
        ExitCode::SUCCESS
    }
}
