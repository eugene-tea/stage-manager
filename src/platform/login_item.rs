//! Launch at login: `SMAppService.mainApp` (the app itself as a login item, listed in System
//! Settings → General → Login Items).
//!
//! Only an installed copy ever registers: the bundle must sit in `/Applications` or
//! `~/Applications` (not translocated, not a build under `target/`, not the dev copy in
//! `~/Library/Application Support/Stage Manager Dev/`), with neither `STAGE_MANAGER_HOME`
//! (test runs) nor `STAGE_MANAGER_NO_LOGIN_ITEM` set. Test runs therefore never add a login
//! item. [`gate`] is the pure rule; [`refusal`] applies it to this process.

use std::path::{Path, PathBuf};

use objc2_foundation::NSBundle;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Set (any value) to never register or unregister a login item.
pub const OPT_OUT_ENV: &str = "STAGE_MANAGER_NO_LOGIN_ITEM";

/// The dev copy's folder under the home folder (`scripts/dev-app.sh`).
pub const DEV_DIR: &str = "Library/Application Support/Stage Manager Dev";

/// Why this copy may not touch login items, or `None` when it may.
pub fn gate(bundle: &Path, home: Option<&Path>, custom_home: bool, opt_out: bool) -> Option<&'static str> {
    if opt_out {
        return Some("STAGE_MANAGER_NO_LOGIN_ITEM is set");
    }
    if custom_home {
        return Some("STAGE_MANAGER_HOME is set (a test run)");
    }
    let text = bundle.to_string_lossy();
    if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return Some("not running from an app bundle");
    }
    if text.contains("/AppTranslocation/") || text.contains("/target/") {
        return Some("not an installed copy");
    }
    if let Some(home) = home
        && bundle.starts_with(home.join(DEV_DIR))
    {
        return Some("the dev copy");
    }
    let in_folder = |dir: &Path| bundle.parent() == Some(dir);
    let installed = in_folder(Path::new("/Applications")) || home.is_some_and(|h| in_folder(&h.join("Applications")));
    if installed { None } else { Some("not in Applications") }
}

/// This process's bundle path.
pub fn bundle_path() -> PathBuf {
    PathBuf::from(NSBundle::mainBundle().bundlePath().to_string())
}

/// Why this process may not touch login items, or `None` when it may.
pub fn refusal() -> Option<&'static str> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    gate(
        &bundle_path(),
        home.as_deref(),
        super::paths::custom_home().is_some(),
        std::env::var_os(OPT_OUT_ENV).is_some(),
    )
}

/// The login item's state as System Settings has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    /// Registered and allowed.
    On,
    /// Registered, but the user must allow it in Login Items.
    NeedsApproval,
    /// Not registered (or turned off in Login Items).
    Off,
    /// This copy may not register (see [`refusal`]).
    Unavailable(&'static str),
}

impl LoginStatus {
    /// Whether the Settings switch shows on.
    pub fn shows_on(self) -> bool {
        matches!(self, Self::On | Self::NeedsApproval)
    }
}

/// The current state (no change made).
pub fn status() -> LoginStatus {
    if let Some(why) = refusal() {
        return LoginStatus::Unavailable(why);
    }
    // SAFETY: class methods without arguments; the service object is retained.
    let status = unsafe { SMAppService::mainAppService().status() };
    match status {
        SMAppServiceStatus::Enabled => LoginStatus::On,
        SMAppServiceStatus::RequiresApproval => LoginStatus::NeedsApproval,
        _ => LoginStatus::Off,
    }
}

/// Registers (`on`) or unregisters the app as a login item. Refused for copies [`gate`]
/// rules out; an error says why.
pub fn set(on: bool) -> Result<(), String> {
    if let Some(why) = refusal() {
        return Err(format!("login item left alone: {why}"));
    }
    // SAFETY: class method without arguments; the service object is retained.
    let service = unsafe { SMAppService::mainAppService() };
    // SAFETY: plain calls on a live SMAppService; errors come back as NSError.
    let result = unsafe { if on { service.registerAndReturnError() } else { service.unregisterAndReturnError() } };
    match result {
        Ok(()) => {
            tracing::info!(on, "login item set");
            Ok(())
        }
        Err(e) => {
            let text = e.localizedDescription().to_string();
            // Unregistering what is not registered is not a failure.
            if !on && unsafe { service.status() } == SMAppServiceStatus::NotRegistered {
                return Ok(());
            }
            tracing::warn!(on, "login item: {text}");
            Err(text)
        }
    }
}

/// Opens System Settings → General → Login Items.
pub fn open_login_items() {
    // SAFETY: class method without arguments.
    unsafe { SMAppService::openSystemSettingsLoginItems() };
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/someone";

    fn g(bundle: &str) -> Option<&'static str> {
        gate(Path::new(bundle), Some(Path::new(HOME)), false, false)
    }

    #[test]
    fn only_an_installed_copy_registers() {
        assert_eq!(g("/Applications/Stage Manager.app"), None);
        assert_eq!(g("/Users/someone/Applications/Stage Manager.app"), None);
        assert!(g("/Users/someone/Library/Application Support/Stage Manager Dev/Stage Manager.app").is_some(), "the dev copy");
        assert!(g("/Users/someone/XGit/stage-manager/target/Stage Manager.app").is_some(), "a build");
        assert!(g("/Applications/Utilities/target/Stage Manager.app").is_some());
        assert!(g("/private/var/folders/xy/T/AppTranslocation/1234/d/Stage Manager.app").is_some(), "run from the disk image");
        assert!(g("/Volumes/Stage Manager/Stage Manager.app").is_some(), "the disk image itself");
        assert!(g("/Users/someone/Downloads/Stage Manager.app").is_some());
        assert!(g("/Users/someone/XGit/stage-manager/target/release").is_some(), "a bare binary (cargo test, cargo run)");
        assert!(g("/Applications/Sub/Stage Manager.app").is_some(), "directly in Applications only");
    }

    #[test]
    fn test_runs_and_the_opt_out_never_register() {
        let installed = Path::new("/Applications/Stage Manager.app");
        assert!(gate(installed, Some(Path::new(HOME)), true, false).is_some(), "STAGE_MANAGER_HOME");
        assert!(gate(installed, Some(Path::new(HOME)), false, true).is_some(), "STAGE_MANAGER_NO_LOGIN_ITEM");
        assert_eq!(gate(installed, None, false, false), None);
    }

    #[test]
    fn this_test_binary_may_never_touch_login_items() {
        assert!(refusal().is_some(), "cargo test runs a bare binary");
        assert!(matches!(status(), LoginStatus::Unavailable(_)));
        assert!(set(true).is_err() && set(false).is_err());
    }

    #[test]
    fn the_switch_shows_on_while_registered() {
        assert!(LoginStatus::On.shows_on() && LoginStatus::NeedsApproval.shows_on());
        assert!(!LoginStatus::Off.shows_on() && !LoginStatus::Unavailable("x").shows_on());
    }
}
