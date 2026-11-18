//! `stage-manager preview <window> [--dark|--light] [--png FILE]`: one of the app's own
//! windows alone, for a look or a picture. It manages nothing: no WM thread, no event
//! monitor, no Accessibility, no login item, no config or macOS setting read or written
//! (built-in defaults and made-up states), and its log goes to stderr only.
//!
//! Windows: `settings`, `settings-apple` (Apple's Stage Manager on, the login item awaiting
//! approval, a config error), `welcome` (both approvals missing), `welcome-apple` (Apple's Stage
//! Manager on too), `welcome-done` (all set), `about`, `update` (a newer release offered),
//! `uptodate`. `--pane general|stage-manager|stages|windows|advanced` opens Settings on that
//! pane (General by default); the preview never remembers one. `--mode
//! accessibility|spaces|apple|paused|relaunch|fell-back` sets what the mode picker of the
//! Advanced pane shows (Accessibility running by default): one of the three modes in force,
//! Apple's Stage Manager on with `"pause"`, Spaces chosen while Accessibility runs, or Spaces
//! fallen back to it.
//!
//! With `--png FILE` the window is drawn into FILE in-process (`cacheDisplayInRect`; never a
//! screen capture) and the command exits without the window ever showing. Without it the
//! window shows and the command exits when it closes.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindow};
use objc2_foundation::{MainThreadMarker, NSTimer};

use super::mode_pick::{ModeFacts, Running};
use super::{about, alert, form, settings_window, shell, welcome};
use crate::config::Config;
use crate::engine::engine_choice::ConfigEngine;
use crate::engine::recovery::EngineKind;
use crate::platform::login_item::LoginStatus;
use crate::platform::native_settings::RawSettings;
use crate::platform::permissions::PermissionState;
use crate::updates;

const WINDOWS: &str = "settings|settings-apple|welcome|welcome-apple|welcome-done|about|update|uptodate";
const MODES: &str = "accessibility|spaces|apple|paused|relaunch|fell-back";

/// The mode picker's facts for `--mode NAME` (module docs).
fn mode_facts(name: &str) -> Option<ModeFacts> {
    let running = |kind, fell_back| Some(Running { kind, fell_back });
    let off = |engine, run| ModeFacts { apple_on: false, follows_apple: false, engine, running: run };
    Some(match name {
        "accessibility" => off(ConfigEngine::Accessibility, running(EngineKind::Corner, false)),
        "spaces" => off(ConfigEngine::Spaces, running(EngineKind::Spaces, false)),
        "apple" => ModeFacts { apple_on: true, follows_apple: true, engine: ConfigEngine::Accessibility, running: running(EngineKind::Corner, false) },
        "paused" => ModeFacts { apple_on: true, follows_apple: false, engine: ConfigEngine::Accessibility, running: running(EngineKind::Corner, false) },
        "relaunch" => off(ConfigEngine::Spaces, running(EngineKind::Corner, false)),
        "fell-back" => off(ConfigEngine::Spaces, running(EngineKind::Corner, true)),
        _ => return None,
    })
}

thread_local! {
    static VALUES: RefCell<Option<settings_window::SettingsValues>> = const { RefCell::new(None) };
}

fn usage() -> ExitCode {
    eprintln!("usage: stage-manager preview <{WINDOWS}> [--dark|--light] [--pane PANE] [--mode {MODES}] [--png FILE]");
    ExitCode::from(2)
}

fn settings_values() -> settings_window::SettingsValues {
    let (version, build) = about::versions();
    let version = format!("{version} ({})", build.unwrap_or_else(|| "412".into()));
    settings_window::SettingsValues::new(
        &Config::default(),
        &RawSettings::new(),
        LoginStatus::On,
        "~/.config/stage-manager/config.toml".into(),
        None,
        version,
    )
}

/// A preview's Settings change: the window follows, nothing is written.
fn preview_change(change: settings_window::Change) {
    tracing::info!(?change, "preview: a change (nothing is written)");
    use settings_window::Change as C;
    let next = VALUES.with(|v| {
        let mut v = v.borrow_mut();
        let values = v.as_mut()?;
        match change {
            C::LaunchAtLogin(on) => values.login = if on { LoginStatus::On } else { LoginStatus::Off },
            C::MenuBarIcon(on) => values.app.show_menu_bar_icon = on,
            C::AutoUpdates(on) => values.app.check_for_updates = on,
            C::InstallUpdates(on) => values.app.install_updates = on,
            C::ShowRecentApps(on) => values.show_recent_apps = on,
            C::AllAtOnce(all) => values.all_at_once = all,
            C::RestoreAfterRestart(on) => values.restore_after_restart = on,
            C::NewAppsJoinCurrent(on) => values.new_apps_join_current = on,
            C::MergeOnDrop(on) => values.merge_on_drop = on,
            C::ShiftBringsHere(on) => values.shift_brings_here = on,
            C::StripOrder(o) => values.strip_order = o,
            C::Fit(m) => values.fit = m,
            C::Hotkeys(on) => values.hotkeys = on,
            C::Speed(s) => values.speed = s,
            C::TurnOffApple => {
                values.apple_on = false;
                values.mode.apple_on = false;
            }
            C::Mode(m) => {
                values.mode = values.mode.after(m);
                values.apple_on = values.mode.apple_on;
                values.follows_apple = values.mode.follows_apple;
            }
            C::CopyAgentInstructions => settings_window::confirm_copied(),
            _ => {}
        }
        Some(values.clone())
    });
    if let Some(values) = next {
        settings_window::apply(values);
    }
}

/// The window to show or draw.
fn make(mtm: MainThreadMarker, which: &str, pane: settings_window::Pane, mode: ModeFacts, drawn: bool) -> Option<Retained<NSWindow>> {
    let missing = PermissionState { accessibility: false, screen_recording: false };
    let granted = PermissionState { accessibility: true, screen_recording: true };
    let log = |a: welcome::WelcomeAction| tracing::info!(?a, "preview: welcome click");
    let current = updates::current();
    Some(match which {
        "settings" => {
            let mut values = settings_values();
            values.mode = mode;
            values.apple_on = mode.apple_on;
            values.follows_apple = mode.follows_apple;
            VALUES.with(|v| *v.borrow_mut() = Some(values.clone()));
            settings_window::show_with_handler(mtm, values, pane, preview_change)
        }
        "settings-apple" => {
            let mut values = settings_values();
            values.apple_on = true;
            values.mode = ModeFacts { apple_on: true, ..mode };
            values.login = LoginStatus::NeedsApproval;
            values.config_error = Some("config.toml: line 12: [strip] width: expected number, 60 to 800".into());
            VALUES.with(|v| *v.borrow_mut() = Some(values.clone()));
            settings_window::show_with_handler(mtm, values, pane, preview_change)
        }
        "welcome" => welcome::create(mtm, welcome::rows(&missing, true, false, false, false), log),
        "welcome-apple" => welcome::create(mtm, welcome::rows(&missing, true, true, true, false), log),
        "welcome-done" => welcome::create(mtm, welcome::rows(&granted, true, false, false, false), log),
        "about" => {
            let w = about::show_and_find(mtm)?;
            if drawn {
                // Drawn, never shown: it leaves before the run loop turns.
                w.orderOut(None);
            }
            w
        }
        "update" => {
            let release = updates::Release {
                version: updates::Version { major: current.major, minor: current.minor + 1, patch: 0, pre: Vec::new() },
                tag: String::new(),
                page: updates::RELEASES_URL.into(),
                dmg: None,
                dmg_size: None,
                notes: "## What's new\n\n- Updates install themselves: Install and Relaunch\n- Windows come back before the relaunch".into(),
            };
            let target = Ok(std::path::PathBuf::from("/Applications/Stage Manager.app"));
            alert::build(mtm, shell::offer_spec(&release, &current, &target), |b| tracing::info!(b, "preview: update button"))
        }
        "uptodate" => {
            let (title, text) = updates::up_to_date_text(&current);
            alert::build(
                mtm,
                alert::AlertSpec { key: "update", title, text, buttons: vec!["OK".into()], cancel: Some(0), warning: false, activate: false },
                |_| {},
            )
        }
        _ => return None,
    })
}

/// Runs the preview.
pub fn run(args: &[String]) -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .try_init();
    let Some(which) = args.first().map(String::as_str) else { return usage() };
    let mut dark = None;
    let mut png: Option<PathBuf> = None;
    let mut pane = settings_window::Pane::default();
    let mut mode = mode_facts("accessibility").unwrap_or_default();
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--dark" => dark = Some(true),
            "--light" => dark = Some(false),
            "--png" => png = rest.next().map(PathBuf::from),
            "--pane" => match rest.next().and_then(|p| settings_window::Pane::from_id(p)) {
                Some(p) => pane = p,
                None => return usage(),
            },
            "--mode" => match rest.next().and_then(|m| mode_facts(m)) {
                Some(m) => mode = m,
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(if png.is_some() { NSApplicationActivationPolicy::Prohibited } else { NSApplicationActivationPolicy::Regular });
    form::set_app_appearance(mtm, dark);
    let Some(window) = make(mtm, which, pane, mode, png.is_some()) else { return usage() };
    if let Some(path) = png {
        window.center();
        return match form::render_png(&window, &path) {
            Ok(()) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("preview: {e}");
                ExitCode::FAILURE
            }
        };
    }
    window.center();
    form::bring_to_front(mtm, &window);
    let watch = RcBlock::new(move |_t: NonNull<NSTimer>| {
        if !window.isVisible() {
            std::process::exit(0);
        }
    });
    // SAFETY: the block runs on the main run loop only.
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.5, true, &watch) };
    app.run();
    ExitCode::SUCCESS
}
