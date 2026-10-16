//! `stage-manager`: the app, its `ctl` client, its guardian and a window-list probe.
//!
//! - `stage-manager` runs the menu bar app.
//! - `stage-manager ctl <words...>` sends one command to the running app's ctl socket.
//! - `stage-manager mark [NOTE...]` writes a problem mark into the day's log (through the
//!   app, with a state dump; without it when the app is not running).
//! - `stage-manager log summary [DATE]` prints a day's counts, marks, warnings and errors;
//!   `log path [DATE]` prints its file.
//! - `stage-manager --guardian <pid>` is the guardian process the app spawns at launch.
//! - `stage-manager --update-helper <pid> <app> <staged> <version> <requirement>` installs a
//!   verified update once the app at `pid` has quit (the app starts it; `platform::updater`).
//! - `stage-manager config check [file] | --print-defaults | --explain <section.key> |
//!   --reference` validates a file (errors with line numbers), prints the commented default
//!   config, explains a key, or prints the configuration table of `docs/CONFIG.md`.
//! - `stage-manager preview settings|settings-apple|welcome|welcome-apple|welcome-done|about|update|uptodate [--dark|--light]
//!   [--png FILE]` opens one of the app's windows alone (nothing managed, no event monitor,
//!   no Accessibility, no login item), or renders it to a PNG and exits.
//! - `stage-manager probe-windows <bundle-id>...` prints those apps' windows as the window
//!   server reports them (bounds, layer, onscreen, z), one JSON line, for test scripts;
//!   `probe-windows --every MS --for MS <bundle-id>...` samples them that often for that
//!   long (each sample with its wall-clock time), one JSON line at the end: the Spaces
//!   check's look at a Desktop slide.
//! - `stage-manager probe-screen` prints the on-screen windows that cover the main display
//!   (a full-screen app's Space), one JSON line, for test scripts.
//! - `stage-manager probe-patch [--out FILE.png] [--rect X,Y,W,H] [--reach PT] [--placement P]` renders what
//!   the wallpaper corner patch would show over the main display's corner right now (the
//!   patch's own code path, from the wallpaper's file; no screen capture) to a PNG (default
//!   `/tmp/stage-manager-patch.png`) and prints what it drew, one JSON line.
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
        Some("mark") => stage_manager::platform::ctl::run_mark(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("log") => stage_manager::platform::logging::command(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("strip-demo") => stage_manager::ui::strip_demo::run(),
        #[cfg(target_os = "macos")]
        Some("preview") => stage_manager::ui::preview::run(&args[2..]),
        Some("config") => {
            let (code, out) = stage_manager::config::command(&args[2..]);
            if code == 0 { print!("{out}") } else { eprint!("{out}") }
            ExitCode::from(code as u8)
        }
        #[cfg(target_os = "macos")]
        Some("--guardian") => stage_manager::platform::guardian::run(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("--update-helper") => stage_manager::platform::updater::run_helper(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-windows") => app::probe_windows(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-screen") => app::probe_screen(),
        #[cfg(target_os = "macos")]
        Some("probe-patch") => stage_manager::ui::patch_probe::run(&args[2..]),
        #[cfg(target_os = "macos")]
        Some("probe-park") => stage_manager::platform::park_probe::run(&args[2..]),
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
                "usage: stage-manager [ctl <command...> | mark [NOTE...] | log <summary|path> [DATE] | config <check [FILE] | --print-defaults | --explain KEY | --reference> | preview <settings|settings-apple|welcome|welcome-apple|welcome-done|about|update|uptodate> [--dark|--light] [--png FILE] | strip-demo | probe-windows <bundle-id>... | probe-screen | probe-patch [--out FILE.png] [--rect X,Y,W,H] | probe-park <fixture-bundle-id> | --version]"
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
    use objc2_foundation::{MainThreadMarker, NSString, NSTimer};
    use serde_json::json;

    use stage_manager::config::{Config, HotkeyConfig};
    use stage_manager::engine::model::Display;
    use stage_manager::platform::apps::{self, WorkspaceObservers};
    use stage_manager::platform::controller::{self, UiUpdate};
    use stage_manager::platform::ctl::{self, Command};
    use stage_manager::platform::desktop;
    use stage_manager::platform::input::{self, InputKind, InputMonitor};
    use stage_manager::platform::instance::{self, Launch};
    use stage_manager::platform::native_settings::{self, RawSettings};
    use stage_manager::platform::permissions::{self, PermissionState};
    use stage_manager::platform::wm_thread::{self, WmCommand, WmHandle};
    use stage_manager::engine::{fullscreen, leftovers};
    use stage_manager::platform::space_keys::{self, Direction, Expose, SpaceKey, SpaceView};
    use stage_manager::platform::{capture, guardian, hotkeys, logging, paths, safety, windows};
    use stage_manager::ui::shell;
    use stage_manager::ui::status_menu::{self, MenuAction, MenuState, StatusMenu};
    use stage_manager::ui::{strip, strip_model};

    /// Permission, settings (Apple's Stage Manager coming on included) and display poll
    /// period.
    const POLL_SECONDS: f64 = 1.0;

    /// How long Quit waits for the WM thread to put `windows` windows back: the action in
    /// flight when Quit came (a batch stops between actions), then a frame per window, each
    /// up to three AX calls (size, position, size) that a slow or hung app may let run to
    /// the 0.25 s messaging timeout, tried again quickly while the app is busy, or a stale
    /// element looked up afresh (at most [`leftovers::window_restore_max`]). At least 4 s, at
    /// most 30 s; plus the leftovers check after the unpark (at most
    /// [`leftovers::LEFTOVERS_MAX`]).
    pub(crate) fn shutdown_timeout(windows: usize) -> Duration {
        let one_try = 3.0 * f64::from(wm_thread::AX_TIMEOUT_SECONDS);
        let per_window = one_try.max(leftovers::window_restore_max().as_secs_f64()) + 0.05;
        let secs = 2.0 + per_window * windows as f64;
        Duration::from_secs_f64(secs.clamp(4.0, 30.0)) + leftovers::LEFTOVERS_MAX
    }

    /// How long exit waits for window captures in flight (see [`capture::quiesce`]).
    const CAPTURE_QUIESCE: Duration = Duration::from_secs(2);

    /// After a signal, the main thread gets the shutdown timeout plus this before the
    /// watcher thread unparks on its own.
    const SIGNAL_GRACE_EXTRA: Duration = Duration::from_secs(2);

    /// The WM thread's handle, for Quit: reachable even while the app state is borrowed
    /// (a menu action runs inside that borrow).
    static WM: OnceLock<WmHandle> = OnceLock::new();

    /// `STAGE_MANAGER_TEST=1` enables test-only ctl commands (`debug-panic`).
    const TEST_ENV: &str = "STAGE_MANAGER_TEST";

    static QUITTING: AtomicBool = AtomicBool::new(false);

    /// How many windows were parked when a logout was announced: a quit that comes while the
    /// head start still brings them back waits as long as their unpark may take.
    static LOGOUT_WINDOWS: AtomicUsize = AtomicUsize::new(0);

    /// When this run started: `ping` says how long ago, so a launch handing off at login does
    /// not open the Settings ([`instance::shows_settings`]).
    static STARTED: OnceLock<Instant> = OnceLock::new();

    struct App {
        menu: StatusMenu,
        wm: WmHandle,
        permissions: PermissionState,
        settings: RawSettings,
        strip_visible: bool,
        config_error: Option<String>,
        last_mark: Option<String>,
        hotkeys: Option<HotkeyConfig>,
        thumbnail_interval_ms: Option<u64>,
        /// Window captures were refused when the menu was last drawn.
        capture_refused: bool,
        display: Option<Display>,
        _workspace: WorkspaceObservers,
        _spaces: WorkspaceObservers,
        _appearance: WorkspaceObservers,
        _power_off: WorkspaceObservers,
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
                capture_on: shell::capture_on(),
                capture_refused: self.capture_refused,
                apple_stage_manager_on: native_settings::apple_stage_manager_enabled(&self.settings),
                config_error: self.config_error.clone(),
                mark_shown: logging::verbose_now(),
                last_mark: self.last_mark.clone(),
            }
        }

        fn refresh_menu(&self) {
            self.menu.update(&self.menu_state());
        }

        /// Runs every second: permissions, `com.apple.WindowManager`, the main display.
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
            let refused = capture::refused();
            if refused != self.capture_refused {
                self.capture_refused = refused;
                self.refresh_menu();
            }
            if let Some(settings) = native_settings::refresh() {
                warn_if_apple_stage_manager(&settings);
                self.settings = settings.clone();
                self.wm.send(WmCommand::NativeSettings(settings));
                self.refresh_menu();
            }
            self.send_display_if_changed();
            shell::poll(self.permissions, &self.settings, self.capture_refused);
            if let Some(mtm) = MainThreadMarker::new() {
                stage_manager::ui::corner_patch::poll(mtm);
            }
        }

        fn send_display_if_changed(&mut self) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let Some(main) = desktop::main_display(mtm) else { return };
            if self.display != Some(main) {
                tracing::info!(frame = %main.frame, visible = %main.visible, corner = ?main.corner, "main display");
                let choice = desktop::park_corner(main.frame, main.visible);
                if choice.overlap > 0.0 {
                    tracing::warn!(
                        corner = ?choice.corner,
                        overlap = choice.overlap.round(),
                        "every corner of the main display has another display beyond it: parked windows may show there; this corner overlaps the least"
                    );
                }
                self.display = Some(main);
                self.wm.send(WmCommand::Displays(vec![main]));
                stage_manager::ui::corner_patch::display_changed(mtm);
            }
        }

        fn on_ui(&mut self, update: UiUpdate) {
            match update {
                UiUpdate::Config { error, hotkeys, thumbnail_interval_ms, capture: on } => {
                    shell::config_changed(error.clone());
                    self.config_error = error;
                    capture::set_enabled(on);
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
                UiUpdate::CornerPatch(plan) => {
                    if let Some(mtm) = MainThreadMarker::new() {
                        stage_manager::ui::corner_patch::apply(mtm, plan);
                    }
                    return;
                }
            }
            self.refresh_menu();
        }

        fn on_menu(&mut self, action: MenuAction) {
            match action {
                MenuAction::Allow(_)
                | MenuAction::About
                | MenuAction::CheckForUpdates
                | MenuAction::Settings
                | MenuAction::TurnOffAppleStageManager => shell::menu(action),
                MenuAction::MarkProblem => {
                    // Off the main thread: the dumps come from the WM thread.
                    let wm = self.wm.clone();
                    let spawned = std::thread::Builder::new().name("mark".into()).spawn(move || {
                        // A panic writing the mark must not take the app down (Fable M2).
                        let Some(reply) = safety::guarded("a problem mark", || mark_problem(&wm, "")) else { return };
                        let at = reply["at"].as_str().map(str::to_string);
                        DispatchQueue::main().exec_async(move || {
                            with_app(|app| {
                                if at.is_some() {
                                    app.last_mark = at;
                                }
                                app.refresh_menu();
                            })
                        });
                    });
                    if let Err(e) = spawned {
                        tracing::warn!("cannot mark a problem: {e}");
                    }
                }
                MenuAction::Quit => quit(),
            }
        }
    }

    fn warn_if_apple_stage_manager(settings: &RawSettings) {
        if native_settings::apple_stage_manager_enabled(settings) {
            tracing::warn!("Apple's Stage Manager is on (GloballyEnabled = 1): standing down until it is off");
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
        // No new window capture from here on; those in flight are waited for at exit.
        capture::begin_shutdown();
        let timeout = shutdown_timeout(safety::registry_len().max(LOGOUT_WINDOWS.load(Ordering::SeqCst)));
        tracing::info!(timeout_ms = timeout.as_millis() as u64, "quitting: unparking every window");
        let ok = match WM.get().map(|wm| wm.shutdown(timeout)) {
            Some(Some(true)) => true,
            Some(Some(false)) => {
                tracing::warn!("some windows did not come back through the WM thread; retrying from the registry");
                registry_then_leftovers("quit")
            }
            Some(None) => {
                tracing::warn!("WM thread did not answer; unparking from the registry");
                registry_then_leftovers("quit")
            }
            None => {
                tracing::warn!("no WM thread; unparking from the registry");
                registry_then_leftovers("quit")
            }
        };
        finish_exit(ok, 0);
    }

    /// The registry's unpark, then the window server's word on it (6X): a registry move that
    /// "succeeded" is not proof, so every window of an allowed app still in the park zone is
    /// brought back or leaves the exit unclean (the guardian waits for it). Bounded: the
    /// registry's quick retries share one budget ([`leftovers::REGISTRY_RETRY_BUDGET`], then
    /// one try a window), the leftovers check at most [`leftovers::LEFTOVERS_MAX`].
    fn registry_then_leftovers(reason: &str) -> bool {
        let registry = safety::emergency_unpark(reason);
        let leftovers = safety::leftovers_back(reason, Default::default());
        registry && leftovers
    }

    fn finish_exit(ok: bool, code: i32) -> ! {
        if ok {
            safety::mark_clean_exit();
        } else {
            tracing::error!("not every window could be restored; the guardian will scan after exit");
        }
        ctl::remove_socket();
        // An update ready to install: its helper waits for this process to exit (8C).
        stage_manager::platform::updater::launch_pending_helper();
        // A capture still in flight at exit makes macOS alert about screen access. Before the
        // counts, so the captures it waits for are in them.
        capture::quiesce(CAPTURE_QUIESCE);
        logging::write_counts("quit");
        tracing::info!("bye");
        std::process::exit(code);
    }

    /// How long a problem mark waits for each of the WM thread's dumps.
    const MARK_DUMP_WAIT: Duration = Duration::from_secs(3);

    /// Writes a problem mark: the note, `ctl state` and `ctl dump` (each, or that the WM
    /// thread did not answer: a hung WM thread is worth marking too) and the recent detailed
    /// lines, window titles hashed unless `[log] titles` is on. Runs off the main thread.
    fn mark_problem(wm: &WmHandle, note: &str) -> serde_json::Value {
        let ask = |cmd: Command| {
            wm.ctl(cmd, MARK_DUMP_WAIT)
                .map(logging::mark_section)
                .unwrap_or_else(|| "the WM thread did not answer".to_string())
        };
        let sections = [("state", ask(Command::State)), ("dump", ask(Command::Dump))];
        match logging::write_mark(note, &sections) {
            Ok(m) => json!({ "ok": true, "at": m.at, "file": m.file }),
            Err(e) => ctl::error_reply(&e),
        }
    }

    fn ctl_handler(wm: WmHandle) -> ctl::Handler {
        Arc::new(move |cmd| match cmd {
            Command::Ping => json!({
                "ok": true,
                "pong": true,
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
                // A launch during a quit waits for it instead of handing off (the review of 8A, M1).
                "quitting": QUITTING.load(Ordering::SeqCst),
                "uptime_ms": STARTED.get().map(|t| t.elapsed().as_millis() as u64),
                "apple_stage_manager_on": native_settings::apple_stage_manager_enabled(&native_settings::current()),
            }),
            Command::Settings => {
                DispatchQueue::main().exec_async(shell::show_settings);
                json!({ "ok": true })
            }
            Command::Quit => {
                DispatchQueue::main().exec_async(quit);
                json!({ "ok": true, "quitting": true })
            }
            Command::Mark(note) => {
                let reply = mark_problem(&wm, &note);
                let at = reply["at"].as_str().map(str::to_string);
                if at.is_some() {
                    DispatchQueue::main().exec_async(move || {
                        with_app(|app| {
                            app.last_mark = at;
                            app.refresh_menu();
                        })
                    });
                }
                reply
            }
            Command::DebugMenuQuit => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-menu-quit needs STAGE_MANAGER_TEST=1");
                }
                // The menu's path: the action runs while the app state is borrowed.
                DispatchQueue::main().exec_async(|| with_app(|app| app.on_menu(MenuAction::Quit)));
                json!({ "ok": true, "quitting": true })
            }
            Command::DebugUpdate => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-update needs STAGE_MANAGER_TEST=1");
                }
                #[cfg(feature = "update-test")]
                {
                    DispatchQueue::main().exec_async(stage_manager::ui::shell::debug_install);
                    json!({ "ok": true, "checking": true })
                }
                #[cfg(not(feature = "update-test"))]
                ctl::error_reply("debug-update needs a build with the update-test feature")
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
            Command::DebugStrip => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-strip needs STAGE_MANAGER_TEST=1");
                }
                let mut probe = serde_json::Value::Null;
                DispatchQueue::main().exec_sync(|| probe = strip::probe());
                json!({ "ok": true, "strip": probe })
            }
            Command::DebugOverlay { region } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-overlay needs STAGE_MANAGER_TEST=1");
                }
                let mut overlay = None;
                DispatchQueue::main().exec_sync(|| overlay = strip::overlay_window());
                let Some((number, origin)) = overlay else {
                    return json!({ "ok": true, "shown": false });
                };
                match stage_manager::platform::self_capture::opaque_bounds(number as u32, origin, region, Duration::from_secs(3)) {
                    Ok(bounds) => json!({ "ok": true, "shown": true, "bounds": bounds }),
                    Err(e) => ctl::error_reply(&e),
                }
            }
            Command::DebugExpose { which } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-expose needs STAGE_MANAGER_TEST=1");
                }
                debug_expose(which)
            }
            Command::DebugDrag { points, shift, hold_ms } => {
                if std::env::var_os(TEST_ENV).is_none() {
                    return ctl::error_reply("debug-drag needs STAGE_MANAGER_TEST=1");
                }
                match input::drag(&points, shift, Duration::from_millis(hold_ms.min(5000))) {
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
        let ok = registry_then_leftovers("signal");
        finish_exit(ok, 1);
    }

    /// Runs the menu bar app until Quit.
    pub fn run() -> ExitCode {
        let _ = STARTED.set(Instant::now());
        logging::init(logging::Process::App);
        safety::install_panic_hook();
        tracing::info!(version = env!("CARGO_PKG_VERSION"), pid = std::process::id(), "Stage Manager starting");
        if let Some(home) = paths::custom_home() {
            tracing::info!("STAGE_MANAGER_HOME={}", home.display());
        }
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::error!("not on the main thread");
            return ExitCode::FAILURE;
        };

        // One copy at a time, claimed before the WM thread, the guardian or any observer
        // starts (Fable M1, Grok High 1): the lock, a copy answering, then the socket bound.
        match instance::claim() {
            Launch::Manage => {}
            Launch::HandOff | Launch::Wait => return ExitCode::SUCCESS,
            Launch::Leave => return ExitCode::FAILURE,
        }
        let listener = match ctl::bind() {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("cannot own the ctl socket ({e}); not managing windows beside another copy");
                return ExitCode::FAILURE;
            }
        };

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
            if let Some(mtm) = MainThreadMarker::new() {
                stage_manager::ui::corner_patch::space_changed(mtm);
            }
            wm_spaces.send(WmCommand::SpaceChanged);
        });
        let appearance = apps::observe_appearance(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                stage_manager::ui::corner_patch::appearance_changed(mtm);
            }
        });

        // The logout's head start (8F, the review's M3): at a logout, restart or shutdown the
        // WM thread brings every window back, checks for leftovers and saves the stages at
        // once, so windows are at their real frames as early as possible before apps save
        // theirs for "Reopen windows when logging back in". The app does not quit then: an app
        // that quits before macOS asks may not be reopened at the next login. macOS's quit
        // (`applicationShouldTerminate:`, as in 1.2.0) finds the work done and only exits; a
        // logout another app cancels ends the hold after a minute (a fresh start).
        let wm_power = wm.clone();
        let power_off = apps::observe_power_off(move || {
            LOGOUT_WINDOWS.fetch_max(safety::registry_len(), Ordering::SeqCst);
            wm_power.send(WmCommand::LogoutComing);
        });

        // No system prompts at launch: the welcome window (or the menu) asks, on a click.
        let permissions = PermissionState::current();
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
        let on_input = move |event: input::InputEvent| {
            if event.is_switcher_key() {
                // ⌘Tab or ⌘` may switch stages: fresh snapshots for the animation (never on
                // other ⌘ shortcuts, which would capture every shown window on each ⌘C).
                strip::prefetch();
            }
            let drop = if event.kind == InputKind::MouseUp { strip::drop_slot(event.at) } else { None };
            wm_input.send(WmCommand::Input(event, drop));
        };
        // Mouse moves go to the strip on this thread: off its cards the strip lets clicks
        // through, so it learns where the pointer is from the global monitor there.
        let input_monitor = input::monitor(mtm, on_input, strip::pointer_moved);
        if input_monitor.is_none() {
            tracing::warn!("cannot monitor input; wallpaper clicks, window drops on the strip and Shift sugar are off");
        }

        if let Err(e) = ctl::serve(listener, ctl_handler(wm.clone())) {
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
            last_mark: None,
            hotkeys: None,
            thumbnail_interval_ms: None,
            capture_refused: false,
            display: None,
            _workspace: workspace,
            _spaces: spaces,
            _appearance: appearance,
            _power_off: power_off,
            _input: input_monitor,
            _timer: timer,
        };
        state.send_display_if_changed();
        state.refresh_menu();
        APP.with(|a| *a.borrow_mut() = Some(state));
        // The loaded config's interval follows in the first UiUpdate::Config.
        strip::install(mtm, Config::default().strip.thumbnail_interval_ms);
        status_menu::set_action_handler(|action| with_app(|app| app.on_menu(action)));
        let hooks = shell::Hooks {
            set_menu_icon: |visible| with_app(|app| app.menu.set_visible(visible)),
            refresh_menu: || with_app(|app| app.refresh_menu()),
            poll_now: || with_app(App::poll),
            quit,
        };
        let raw = native_settings::current();
        shell::install(mtm, wm.clone(), hooks, permissions, raw);

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
        let usage = || {
            eprintln!("usage: stage-manager probe-windows [--every MS --for MS] <bundle-id>...");
            ExitCode::from(2)
        };
        let (sampling, args) = match args {
            [every, e, r#for, f, rest @ ..] if every == "--every" && r#for == "--for" => {
                match (e.parse::<u64>(), f.parse::<u64>()) {
                    (Ok(e), Ok(f)) if e > 0 => (Some((Duration::from_millis(e), Duration::from_millis(f))), rest),
                    _ => return usage(),
                }
            }
            _ => (None, args),
        };
        if args.is_empty() {
            return usage();
        }
        let pids: Vec<i32> = args
            .iter()
            .flat_map(|bundle| NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle)))
            .map(|a| a.processIdentifier())
            .collect();
        let look = || {
            let list = windows::list_windows();
            let z = windows::z_order(&list);
            list.iter()
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
                .collect::<Vec<_>>()
        };
        let mut samples = Vec::new();
        if let Some((every, span)) = sampling {
            let start = Instant::now();
            let mut due = start;
            while start.elapsed() <= span {
                let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                samples.push(json!({ "t": t, "windows": look() }));
                due += every;
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
        }
        let out = look();
        // The main display, with its visible frame (the park zone sits above a bottom Dock).
        let mut display = json!(desktop::cg_display_bounds().first().copied());
        if let (Some(mtm), Some(obj)) = (MainThreadMarker::new(), display.as_object_mut())
            && let Some(main) = desktop::main_display(mtm)
        {
            obj.insert("visible".into(), json!(main.visible));
        }
        let mut reply = json!({ "ok": true, "pids": pids, "display": display, "windows": out });
        if sampling.is_some()
            && let Some(obj) = reply.as_object_mut()
        {
            obj.insert("samples".into(), json!(samples));
        }
        println!("{reply}");
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
