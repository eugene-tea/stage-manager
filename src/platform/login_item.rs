//! Launch at login (round early-start): a launchd agent, so Stage Manager starts as the login
//! session begins, before loginwindow reopens the apps of the last session, not after them.
//!
//! - **The agent:** `SMAppService.agentServiceWithPlistName` with [`AGENT_PLIST`], which
//!   `scripts/bundle.sh` puts in the bundle's `Contents/Library/LaunchAgents` (sealed by its
//!   signature): `BundleProgram` the app's own executable, `RunAtLoad`, no `KeepAlive` (the
//!   guardian covers a crash; a relaunch could come while the screen is locked), listed in
//!   System Settings → General → Login Items under the app's name.
//! - **The fallback:** `SMAppService.mainApp`, the app as a login item, which macOS opens only
//!   after the reopened apps (23 and 35 s after the login on his Mac, 11-06): used when macOS
//!   refuses the agent or it awaits the user's approval (logged, and Settings says so,
//!   [`LoginStatus::Late`]).
//! - **One switch:** on registers the agent and unregisters `mainApp`; off unregisters both
//!   ([`next_step`], the pure rule; [`set`] runs it). An install that had `mainApp` on is moved
//!   over once ([`move_to_agent`]); later launches only drop a `mainApp` left beside an
//!   enabled agent ([`tidy`]), so nothing starts twice and a choice made in System Settings
//!   stands.
//! - **Unregistering the agent kills its running job** (Apple's header for
//!   `unregisterAndReturnError`). While this process is that job ([`Started::Agent`]), turning
//!   the switch off waits until the quit ([`at_exit`]), so the switch never quits the app.
//!
//! Only an installed copy ever registers: the bundle must sit in `/Applications` or
//! `~/Applications` (not translocated, not a build under `target/`, not the dev copy in
//! `~/Library/Application Support/Stage Manager Dev/`), with neither `STAGE_MANAGER_HOME`
//! (test runs) nor `STAGE_MANAGER_NO_LOGIN_ITEM` set. Test runs therefore never add a login
//! item or an agent. [`gate`] is the pure rule; [`refusal`] applies it to this process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use objc2::rc::Retained;
use objc2_foundation::{NSBundle, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Set (any value) to never register or unregister a login item.
pub const OPT_OUT_ENV: &str = "STAGE_MANAGER_NO_LOGIN_ITEM";

/// The dev copy's folder under the home folder (`scripts/dev-app.sh`).
pub const DEV_DIR: &str = "Library/Application Support/Stage Manager Dev";

/// The agent's plist, in the bundle's `Contents/Library/LaunchAgents`
/// (`packaging/dev.eugene.stagemanager.agent.plist`).
pub const AGENT_PLIST: &str = "dev.eugene.stagemanager.agent.plist";

/// The agent's launchd label (the plist's `Label`): launchd names a process it starts for the
/// job by it in `XPC_SERVICE_NAME`.
pub const AGENT_LABEL: &str = "dev.eugene.stagemanager.agent";

/// A launch through LaunchServices this soon after the login is the login's (the login item,
/// or macOS reopening the app), not the user's ([`Started::AtLogin`]).
pub const AT_LOGIN: Duration = Duration::from_secs(90);

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

// ----- how this copy was started ---------------------------------------------------------

/// How this process was started, for the launch's line and the hand-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// By launchd, as the agent's job: at the session's start (or at its registration).
    Agent,
    /// Through LaunchServices within [`AT_LOGIN`] of the login: the login item, or macOS
    /// reopening the app (the two cannot be told apart).
    AtLogin,
    /// Anything else: Finder, Spotlight, `open`, a terminal, an update's relaunch.
    ByHand,
}

impl Started {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "launchd agent",
            Self::AtLogin => "login item",
            Self::ByHand => "by hand",
        }
    }
}

/// How a process was started, from its `XPC_SERVICE_NAME` (launchd's job label: the agent's
/// for its job; `application.<bundle id>.…` for an app LaunchServices opened) and how long
/// after the login it started.
pub fn started_from(xpc_service: Option<&str>, since_login: Option<Duration>) -> Started {
    if xpc_service == Some(AGENT_LABEL) {
        Started::Agent
    } else if since_login.is_some_and(|d| d <= AT_LOGIN) {
        Started::AtLogin
    } else {
        Started::ByHand
    }
}

/// How this process was started (read at its first call; the login's time is read then too).
pub fn started() -> Started {
    static STARTED: std::sync::OnceLock<Started> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        let xpc = std::env::var("XPC_SERVICE_NAME").ok();
        started_from(xpc.as_deref(), since_login())
    })
}

/// How long ago this login session began ([`super::apps::login_started`]).
pub fn since_login() -> Option<Duration> {
    super::apps::login_started().map(|l| l.elapsed())
}

// ----- the rule ---------------------------------------------------------------------------

/// One of the two ways to launch at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// The launchd agent ([`AGENT_PLIST`]).
    Agent,
    /// `SMAppService.mainApp`: the app as a login item.
    MainApp,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Self::Agent => "launchd agent",
            Self::MainApp => "login item",
        }
    }
}

/// A service's state as System Settings has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svc {
    /// Registered and allowed.
    Enabled,
    /// Registered; the user must allow it in Login Items (or turned it off there).
    NeedsApproval,
    /// Not registered.
    NotRegistered,
    /// macOS knows no such service (the agent's plist missing from the bundle, say).
    NotFound,
}

impl Svc {
    /// True when registered, allowed or not.
    pub fn registered(self) -> bool {
        matches!(self, Self::Enabled | Self::NeedsApproval)
    }

    fn of(status: SMAppServiceStatus) -> Self {
        match status {
            SMAppServiceStatus::Enabled => Self::Enabled,
            SMAppServiceStatus::RequiresApproval => Self::NeedsApproval,
            SMAppServiceStatus::NotRegistered => Self::NotRegistered,
            _ => Self::NotFound,
        }
    }
}

/// One step towards the switch's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Register(Service),
    Unregister(Service),
}

/// The next step for the switch `on`, given each service's state, or `None` when there:
/// on, the agent enabled and `mainApp` not registered (or, the agent `refused`, `mainApp`
/// registered); off, neither registered. An agent awaiting approval counts as refused
/// (registering it again only fails), and `mainApp` stands in for it, its approval still
/// pending in Login Items. Never both enabled: both would start the app at each login.
pub fn next_step(on: bool, agent: Svc, main: Svc, refused: bool) -> Option<Op> {
    if !on {
        return if agent.registered() {
            Some(Op::Unregister(Service::Agent))
        } else if main.registered() {
            Some(Op::Unregister(Service::MainApp))
        } else {
            None
        };
    }
    if agent == Svc::Enabled {
        return main.registered().then_some(Op::Unregister(Service::MainApp));
    }
    if refused || agent == Svc::NeedsApproval {
        return (!main.registered()).then_some(Op::Register(Service::MainApp));
    }
    Some(Op::Register(Service::Agent))
}

/// Whether the one-time move of an earlier version's login item to the agent is due: the
/// login item is on and the agent not yet. A login item turned off in Login Items (awaiting
/// approval) or never registered is the user's choice and stays.
pub fn move_due(agent: Svc, main: Svc) -> bool {
    main == Svc::Enabled && agent != Svc::Enabled
}

/// The state Settings shows for the two services' states.
pub fn status_from(agent: Svc, main: Svc) -> LoginStatus {
    match (agent, main) {
        (Svc::Enabled, _) => LoginStatus::On,
        (Svc::NeedsApproval, Svc::Enabled) => LoginStatus::Late,
        (_, Svc::Enabled) => LoginStatus::On,
        (Svc::NeedsApproval, _) | (_, Svc::NeedsApproval) => LoginStatus::NeedsApproval,
        _ => LoginStatus::Off,
    }
}

/// macOS's side of the rule: the two services' states and their (un)registration. A fake in
/// the tests.
pub trait Services {
    fn status(&self, s: Service) -> Svc;
    fn register(&mut self, s: Service) -> Result<(), String>;
    fn unregister(&mut self, s: Service) -> Result<(), String>;
}

/// What [`run`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The agent was refused (or awaits approval) and `mainApp` stands in.
    pub fell_back: Option<String>,
    /// Unregistering the agent waits for the quit: this process is its job.
    pub deferred: bool,
}

/// Brings the services to the switch `on` ([`next_step`], at most a few steps), on `sys`.
/// `agent_job`: this process is the agent's running job, which unregistering it would kill:
/// that step is left for the quit ([`Outcome::deferred`]).
pub fn run(on: bool, sys: &mut impl Services, agent_job: bool) -> Result<Outcome, String> {
    let mut out = Outcome::default();
    let mut refused: Option<String> = None;
    for _ in 0..6 {
        let agent = if out.deferred { Svc::NotRegistered } else { sys.status(Service::Agent) };
        let main = sys.status(Service::MainApp);
        let Some(op) = next_step(on, agent, main, refused.is_some()) else {
            if on && agent != Svc::Enabled {
                out.fell_back = Some(refused.unwrap_or_else(|| "it awaits approval in Login Items".into()));
            }
            return Ok(out);
        };
        match op {
            Op::Unregister(Service::Agent) if agent_job => out.deferred = true,
            Op::Register(Service::Agent) => {
                if let Err(e) = sys.register(Service::Agent) {
                    refused = Some(e);
                } else if sys.status(Service::Agent) != Svc::Enabled {
                    refused = Some("it awaits approval in Login Items".into());
                }
            }
            Op::Register(s) => sys.register(s)?,
            Op::Unregister(s) => {
                // Unregistering what is not registered is not a failure.
                if let Err(e) = sys.unregister(s)
                    && sys.status(s).registered()
                {
                    return Err(e);
                }
            }
        }
    }
    Err("launch at login: the services did not settle".into())
}

// ----- macOS -------------------------------------------------------------------------------

/// The login item's state as System Settings has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    /// Registered and allowed (the agent; or the login item, the agent refused outright).
    On,
    /// The login item is on while the agent awaits the user's approval in Login Items:
    /// Stage Manager starts after the apps macOS reopens until it is allowed.
    Late,
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
        matches!(self, Self::On | Self::Late | Self::NeedsApproval)
    }
}

/// The services of this app's bundle.
struct System;

impl System {
    fn service(s: Service) -> Retained<SMAppService> {
        match s {
            // SAFETY: class methods; the name is a plain NSString; the service object is retained.
            Service::Agent => unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(AGENT_PLIST)) },
            // SAFETY: as above, no arguments.
            Service::MainApp => unsafe { SMAppService::mainAppService() },
        }
    }
}

impl Services for System {
    fn status(&self, s: Service) -> Svc {
        // SAFETY: a plain getter on a live SMAppService.
        Svc::of(unsafe { Self::service(s).status() })
    }

    fn register(&mut self, s: Service) -> Result<(), String> {
        // SAFETY: a plain call on a live SMAppService; errors come back as NSError.
        let r = unsafe { Self::service(s).registerAndReturnError() }.map_err(|e| e.localizedDescription().to_string());
        match &r {
            Ok(()) => tracing::info!(service = s.name(), "launch at login: registered"),
            Err(e) => tracing::warn!(service = s.name(), "launch at login: cannot register: {e}"),
        }
        r
    }

    fn unregister(&mut self, s: Service) -> Result<(), String> {
        // SAFETY: as above.
        let r = unsafe { Self::service(s).unregisterAndReturnError() }.map_err(|e| e.localizedDescription().to_string());
        match &r {
            Ok(()) => tracing::info!(service = s.name(), "launch at login: unregistered"),
            Err(e) => tracing::debug!(service = s.name(), "launch at login: unregister: {e}"),
        }
        r
    }
}

/// Unregistering the agent waits for the quit ([`at_exit`]).
static UNREGISTER_AT_EXIT: AtomicBool = AtomicBool::new(false);

/// The current state (no change made).
pub fn status() -> LoginStatus {
    if let Some(why) = refusal() {
        return LoginStatus::Unavailable(why);
    }
    let agent = if UNREGISTER_AT_EXIT.load(Ordering::SeqCst) { Svc::NotRegistered } else { System.status(Service::Agent) };
    status_from(agent, System.status(Service::MainApp))
}

/// Sets the switch: `on` registers the agent (the login item instead when macOS refuses it)
/// and unregisters the login item; off unregisters both. Refused for copies [`gate`] rules
/// out; an error says why.
pub fn set(on: bool) -> Result<(), String> {
    if let Some(why) = refusal() {
        return Err(format!("login item left alone: {why}"));
    }
    if on {
        UNREGISTER_AT_EXIT.store(false, Ordering::SeqCst);
    }
    let agent_job = started() == Started::Agent;
    match run(on, &mut System, agent_job) {
        Ok(out) => {
            if let Some(why) = &out.fell_back {
                tracing::warn!("launch at login: the launchd agent is not in use ({why}); a login item instead, which starts after the apps macOS reopens");
            }
            if out.deferred {
                UNREGISTER_AT_EXIT.store(true, Ordering::SeqCst);
                tracing::info!("launch at login: off; the launchd agent is unregistered at the quit (this copy is its job, which unregistering ends)");
            }
            tracing::info!(on, status = ?status(), "launch at login set");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(on, "launch at login: {e}");
            Err(e)
        }
    }
}

/// An earlier version's login item, still on, moved to the agent: once per install (the
/// caller remembers it). Returns whether this copy may register at all (so the caller does
/// not mark the move done for a copy that may not).
pub fn move_to_agent() -> bool {
    if refusal().is_some() {
        return false;
    }
    let (agent, main) = (System.status(Service::Agent), System.status(Service::MainApp));
    if move_due(agent, main) {
        tracing::info!("launch at login: the login item moves to a launchd agent, which starts with the session");
        let _ = set(true);
    }
    true
}

/// At a launch: a login item left registered beside an enabled agent (the user allowed the
/// agent in Login Items after the fallback) is unregistered, so nothing starts twice.
pub fn tidy() {
    if refusal().is_some() {
        return;
    }
    if System.status(Service::Agent) == Svc::Enabled && System.status(Service::MainApp).registered() {
        tracing::info!("launch at login: the launchd agent is allowed now; the login item that stood in for it goes");
        let _ = System.unregister(Service::MainApp);
    }
}

/// At the quit: the agent's unregistering left for it ([`set`] while this copy is the
/// agent's job). Call last: it ends this process's job.
pub fn at_exit() {
    if UNREGISTER_AT_EXIT.swap(false, Ordering::SeqCst) && refusal().is_none() {
        let _ = System.unregister(Service::Agent);
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
        assert!(!move_to_agent(), "no move from a test");
        tidy();
        UNREGISTER_AT_EXIT.store(true, Ordering::SeqCst);
        at_exit();
        assert!(!UNREGISTER_AT_EXIT.load(Ordering::SeqCst));
    }

    #[test]
    fn the_switch_shows_on_while_registered() {
        assert!(LoginStatus::On.shows_on() && LoginStatus::NeedsApproval.shows_on() && LoginStatus::Late.shows_on());
        assert!(!LoginStatus::Off.shows_on() && !LoginStatus::Unavailable("x").shows_on());
    }

    #[test]
    fn the_agent_names_its_own_job_and_a_launch_soon_after_the_login_is_the_logins() {
        let s = Duration::from_secs;
        assert_eq!(started_from(Some(AGENT_LABEL), Some(s(2))), Started::Agent);
        assert_eq!(started_from(Some(AGENT_LABEL), None), Started::Agent, "launchd's word, whatever the clock");
        assert_eq!(started_from(Some("application.dev.eugene.stagemanager.1.2"), Some(s(30))), Started::AtLogin);
        assert_eq!(started_from(None, Some(AT_LOGIN)), Started::AtLogin);
        assert_eq!(started_from(Some("application.dev.eugene.stagemanager.1.2"), Some(s(600))), Started::ByHand);
        assert_eq!(started_from(Some("0"), None), Started::ByHand, "a terminal, no login time");
    }

    /// macOS as the tests have it: each service's state, the agent refused or left
    /// awaiting approval at registration when asked.
    struct Fake {
        agent: Svc,
        main: Svc,
        refuse_agent: bool,
        agent_needs_approval: bool,
        calls: Vec<Op>,
    }

    impl Fake {
        fn new(agent: Svc, main: Svc) -> Self {
            Self { agent, main, refuse_agent: false, agent_needs_approval: false, calls: Vec::new() }
        }
        fn slot(&mut self, s: Service) -> &mut Svc {
            match s {
                Service::Agent => &mut self.agent,
                Service::MainApp => &mut self.main,
            }
        }
    }

    impl Services for Fake {
        fn status(&self, s: Service) -> Svc {
            match s {
                Service::Agent => self.agent,
                Service::MainApp => self.main,
            }
        }
        fn register(&mut self, s: Service) -> Result<(), String> {
            self.calls.push(Op::Register(s));
            if s == Service::Agent && self.refuse_agent {
                return Err("Operation not permitted".into());
            }
            let approval = s == Service::Agent && self.agent_needs_approval;
            *self.slot(s) = if approval { Svc::NeedsApproval } else { Svc::Enabled };
            Ok(())
        }
        fn unregister(&mut self, s: Service) -> Result<(), String> {
            self.calls.push(Op::Unregister(s));
            if !self.status(s).registered() {
                return Err("Job not found".into());
            }
            *self.slot(s) = Svc::NotRegistered;
            Ok(())
        }
    }

    use Service::{Agent, MainApp};

    #[test]
    fn a_registered_login_item_moves_to_the_agent() {
        let mut sys = Fake::new(Svc::NotRegistered, Svc::Enabled);
        assert!(move_due(sys.agent, sys.main));
        let out = run(true, &mut sys, false).expect("runs");
        assert_eq!((sys.agent, sys.main), (Svc::Enabled, Svc::NotRegistered), "the agent alone");
        assert_eq!(sys.calls, vec![Op::Register(Agent), Op::Unregister(MainApp)], "registered first, then the login item goes");
        assert_eq!(out, Outcome::default());
        assert_eq!(status_from(sys.agent, sys.main), LoginStatus::On);
        assert!(!move_due(sys.agent, sys.main), "once");
    }

    #[test]
    fn a_login_item_turned_off_in_system_settings_is_not_moved() {
        assert!(!move_due(Svc::NotRegistered, Svc::NeedsApproval));
        assert!(!move_due(Svc::NotRegistered, Svc::NotRegistered));
        assert!(!move_due(Svc::Enabled, Svc::Enabled), "the agent is on already: tidy drops the login item");
    }

    #[test]
    fn a_refused_agent_falls_back_to_the_login_item() {
        let mut sys = Fake::new(Svc::NotRegistered, Svc::NotRegistered);
        sys.refuse_agent = true;
        let out = run(true, &mut sys, false).expect("runs");
        assert_eq!((sys.agent, sys.main), (Svc::NotRegistered, Svc::Enabled));
        assert_eq!(out.fell_back.as_deref(), Some("Operation not permitted"), "logged with macOS's reason");
        assert_eq!(status_from(sys.agent, sys.main), LoginStatus::On, "refused outright: nothing for the user to allow");

        // Refused while moving over: the login item stays as it was.
        let mut sys = Fake::new(Svc::NotRegistered, Svc::Enabled);
        sys.refuse_agent = true;
        run(true, &mut sys, false).expect("runs");
        assert_eq!((sys.agent, sys.main), (Svc::NotRegistered, Svc::Enabled));
        assert_eq!(sys.calls, vec![Op::Register(Agent)], "the login item never unregistered");
    }

    #[test]
    fn an_agent_awaiting_approval_falls_back_and_settings_says_so() {
        let mut sys = Fake::new(Svc::NotRegistered, Svc::NotRegistered);
        sys.agent_needs_approval = true;
        let out = run(true, &mut sys, false).expect("runs");
        assert_eq!((sys.agent, sys.main), (Svc::NeedsApproval, Svc::Enabled), "the agent waits for its approval; the login item starts the app meanwhile");
        assert!(out.fell_back.is_some());
        assert_eq!(status_from(sys.agent, sys.main), LoginStatus::Late);
        // Turned off in Login Items earlier: not registered again, the login item stands in.
        let mut sys = Fake::new(Svc::NeedsApproval, Svc::NotRegistered);
        run(true, &mut sys, false).expect("runs");
        assert_eq!(sys.calls, vec![Op::Register(MainApp)]);
        // Allowed later: the next turn on (or tidy at a launch) drops the login item.
        assert_eq!(next_step(true, Svc::Enabled, Svc::Enabled, false), Some(Op::Unregister(MainApp)));
    }

    #[test]
    fn off_is_neither() {
        for (agent, main) in [(Svc::Enabled, Svc::NotRegistered), (Svc::NeedsApproval, Svc::Enabled), (Svc::Enabled, Svc::Enabled), (Svc::NotRegistered, Svc::NeedsApproval)] {
            let mut sys = Fake::new(agent, main);
            let out = run(false, &mut sys, false).expect("runs");
            assert!(!sys.agent.registered() && !sys.main.registered(), "{agent:?} {main:?}");
            assert_eq!(status_from(sys.agent, sys.main), LoginStatus::Off);
            assert!(!out.deferred);
        }
        let mut sys = Fake::new(Svc::NotRegistered, Svc::NotRegistered);
        run(false, &mut sys, false).expect("nothing to do is no failure");
        assert!(sys.calls.is_empty());
    }

    #[test]
    fn off_from_the_agents_own_job_waits_for_the_quit() {
        let mut sys = Fake::new(Svc::Enabled, Svc::Enabled);
        let out = run(false, &mut sys, true).expect("runs");
        assert!(out.deferred, "unregistering would end this process");
        assert_eq!((sys.agent, sys.main), (Svc::Enabled, Svc::NotRegistered), "the login item goes now, the agent at the quit");
        assert_eq!(sys.calls, vec![Op::Unregister(MainApp)]);
    }

    #[test]
    fn on_again_is_a_no_op_and_never_both_are_enabled() {
        let mut sys = Fake::new(Svc::Enabled, Svc::NotRegistered);
        run(true, &mut sys, false).expect("runs");
        assert!(sys.calls.is_empty());
        for agent in [Svc::Enabled, Svc::NeedsApproval, Svc::NotRegistered, Svc::NotFound] {
            for main in [Svc::Enabled, Svc::NeedsApproval, Svc::NotRegistered, Svc::NotFound] {
                for refuse in [false, true] {
                    let mut sys = Fake::new(agent, main);
                    sys.refuse_agent = refuse;
                    run(true, &mut sys, false).expect("settles");
                    assert!(!(sys.agent == Svc::Enabled && sys.main.registered()), "{agent:?} {main:?} refuse={refuse}");
                    assert!(sys.agent == Svc::Enabled || sys.main.registered(), "on means one of them: {agent:?} {main:?} refuse={refuse}");
                }
            }
        }
    }
}
