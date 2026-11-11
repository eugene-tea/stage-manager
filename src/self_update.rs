//! Installing an update, pure: what a downloaded app must prove before it may replace the
//! running one, where it may go, and the swap as a transaction that puts the old app back on
//! any failure (or, when even that fails, says where each copy is).
//!
//! The platform side (`platform::updater`) downloads the release's disk image, mounts it
//! read-only, runs `codesign` and `spctl`, and hands their output here; the helper process it
//! starts at quit runs [`run_swap`] with real file operations.
//!
//! **Trust comes from the running app's own signature**, never from a string compiled in:
//! its team ID and designated requirement are read from its bundle at update time
//! ([`Trust::from_own`]). A candidate must satisfy that designated requirement (the same
//! test macOS applies to the Accessibility and Screen Recording approvals, so they carry
//! over), be signed by a Developer ID of the same team with the same bundle ID, be accepted
//! by Gatekeeper as notarized, and carry a strictly newer version ([`judge`]).

use std::path::{Path, PathBuf};

use crate::updates::Version;

/// The app bundle's name; an update only ever replaces a bundle of this name.
pub const APP_NAME: &str = "Stage Manager.app";
/// The hidden working folder beside the installed app (same volume, so the swap is a rename).
pub const WORK_DIR: &str = ".Stage Manager update";
/// Where the three-rename exchange sets the installed app aside, in the working folder.
pub const ASIDE_NAME: &str = "old.app";
/// A file the helper leaves in the working folder when it holds a copy of the app that could
/// not be put back: the folder is kept (never cleaned at launch) until the next update.
pub const KEEP_MARK: &str = "kept.txt";
/// Gatekeeper's word for a notarized Developer ID app.
pub const NOTARIZED_SOURCE: &str = "Notarized Developer ID";

/// A code signature as `codesign -dv --verbose=2` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeInfo {
    /// `Identifier=`.
    pub identifier: Option<String>,
    /// `TeamIdentifier=` (absent or "not set": none).
    pub team_id: Option<String>,
    /// `Authority=` lines, leaf first.
    pub authorities: Vec<String>,
    /// `Signature=adhoc`.
    pub adhoc: bool,
}

/// Reads `codesign -dv --verbose=2` output (it writes to stderr).
pub fn parse_codesign_display(text: &str) -> CodeInfo {
    let mut info = CodeInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "Identifier" => info.identifier = Some(value.to_string()),
            "TeamIdentifier" if value != "not set" && !value.is_empty() => info.team_id = Some(value.to_string()),
            "Authority" => info.authorities.push(value.to_string()),
            "Signature" if value == "adhoc" => info.adhoc = true,
            _ => {}
        }
    }
    info
}

/// The designated requirement from `codesign -d -r-` output (`designated => …`).
pub fn parse_designated(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("designated => "))
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// A team ID as Apple issues them: ten capital letters or digits.
pub fn valid_team_id(s: &str) -> bool {
    s.len() == 10 && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A reverse-DNS bundle ID: letters, digits, dots and hyphens.
pub fn valid_bundle_id(s: &str) -> bool {
    !s.is_empty() && s.contains('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Signed by a Developer ID Application certificate that chains to Apple's root, with a team.
pub fn is_developer_id(info: &CodeInfo) -> bool {
    !info.adhoc
        && info.team_id.as_deref().is_some_and(valid_team_id)
        && info.authorities.first().is_some_and(|a| a.starts_with("Developer ID Application:"))
        && info.authorities.last().is_some_and(|a| a == "Apple Root CA")
}

/// A Developer ID requirement for `bundle_id` signed by `team` (Apple's own form).
pub fn developer_id_requirement(bundle_id: &str, team: &str) -> String {
    format!(
        "identifier \"{bundle_id}\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// Gatekeeper's answer (`spctl --assess -vv`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    /// `<path>: accepted` and a zero exit.
    pub accepted: bool,
    /// `source=`.
    pub source: Option<String>,
}

/// Reads `spctl --assess -vv` output (stderr) and whether it exited 0.
pub fn parse_spctl(text: &str, exit_ok: bool) -> Assessment {
    let accepted = exit_ok && text.lines().any(|l| l.trim_end().ends_with(": accepted"));
    let source = text.lines().find_map(|l| l.trim().strip_prefix("source=")).map(|s| s.trim().to_string());
    Assessment { accepted, source }
}

/// Accepted as notarized.
pub fn notarized(a: &Assessment) -> bool {
    a.accepted && a.source.as_deref() == Some(NOTARIZED_SOURCE)
}

/// Why this copy of the app cannot install an update itself (it offers Download instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not running from an app bundle (`cargo run`, a bare binary).
    NotABundle,
    /// macOS runs it from a randomized read-only copy (App Translocation): moved out of
    /// Downloads without the Finder, or still quarantined.
    Translocated,
    /// Running from the disk image itself.
    OnDiskImage,
    /// The folder it lives in is not writable by this user.
    NotWritable(PathBuf),
    /// This build is not signed with a Developer ID (a local or development build).
    NotDeveloperId,
    /// Its own signature could not be read.
    NoOwnSignature(String),
}

impl Refusal {
    /// One sentence for the alert.
    pub fn sentence(&self) -> String {
        match self {
            Refusal::NotABundle | Refusal::NotDeveloperId | Refusal::NoOwnSignature(_) => {
                "This copy can't install updates itself; download the new version instead.".into()
            }
            Refusal::Translocated | Refusal::OnDiskImage => {
                "Move Stage Manager to your Applications folder so it can update itself; for now, download the new version.".into()
            }
            Refusal::NotWritable(dir) => format!(
                "Stage Manager can't replace itself in {} (no permission there), so the new version's disk image opens instead: drag Stage Manager to Applications.",
                dir.display()
            ),
        }
    }

    /// Whether the verified disk image is worth opening for a manual install (the app could
    /// check it but cannot put it in place).
    pub fn opens_disk_image(&self) -> bool {
        matches!(self, Refusal::NotWritable(_))
    }
}

/// Where the running bundle may be replaced: its own path, when it is an `.app` named
/// [`APP_NAME`] in a writable folder that is neither translocated nor on a disk image.
pub fn install_target(bundle: &Path, parent_writable: bool) -> Result<PathBuf, Refusal> {
    if bundle.extension().is_none_or(|e| e != "app") || bundle.file_name().is_none_or(|n| n != APP_NAME) {
        return Err(Refusal::NotABundle);
    }
    let text = bundle.to_string_lossy();
    if text.contains("/AppTranslocation/") {
        return Err(Refusal::Translocated);
    }
    if bundle.starts_with("/Volumes") {
        return Err(Refusal::OnDiskImage);
    }
    let Some(parent) = bundle.parent().filter(|p| !p.as_os_str().is_empty()) else { return Err(Refusal::NotABundle) };
    if !parent_writable {
        return Err(Refusal::NotWritable(parent.to_path_buf()));
    }
    Ok(bundle.to_path_buf())
}

/// The working folder and the staged copy's path for `target`.
pub fn staging_paths(target: &Path) -> Option<(PathBuf, PathBuf)> {
    let work = target.parent()?.join(WORK_DIR);
    let staged = work.join(APP_NAME);
    Some((work, staged))
}

/// What the running app trusts in an update: its own bundle ID, team and the requirement a
/// candidate must satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// The bundle ID a candidate must carry.
    pub bundle_id: String,
    /// Our team (Developer ID), read from the running app's own signature.
    pub team_id: Option<String>,
    /// `codesign -R` text: the running app's designated requirement, and (when Developer ID)
    /// the Developer ID requirement for our team.
    pub requirement: String,
    /// A Developer ID signature and notarization are required (always, except in builds with
    /// the `update-test` feature running from a local signature).
    pub strict: bool,
}

impl Trust {
    /// The trust a running app with signature `own`, designated requirement `own_dr` and
    /// bundle ID `bundle_id` extends to updates. `allow_local` (test builds only) accepts a
    /// non-Developer-ID running app, which then trusts only its own designated requirement.
    pub fn from_own(own: &CodeInfo, own_dr: Option<&str>, bundle_id: &str, allow_local: bool) -> Result<Self, Refusal> {
        if !valid_bundle_id(bundle_id) || own.identifier.as_deref() != Some(bundle_id) {
            return Err(Refusal::NoOwnSignature(format!("signed identifier {:?} is not the bundle ID {bundle_id:?}", own.identifier)));
        }
        let dr = own_dr.ok_or_else(|| Refusal::NoOwnSignature("no designated requirement".into()))?;
        if dr.contains('\n') || dr.contains('\0') {
            return Err(Refusal::NoOwnSignature("a malformed designated requirement".into()));
        }
        if is_developer_id(own) {
            let team = own.team_id.clone().unwrap_or_default();
            let requirement = format!("({dr}) and ({})", developer_id_requirement(bundle_id, &team));
            return Ok(Self { bundle_id: bundle_id.to_string(), team_id: Some(team), requirement, strict: true });
        }
        if allow_local && !own.adhoc {
            return Ok(Self { bundle_id: bundle_id.to_string(), team_id: None, requirement: dr.to_string(), strict: false });
        }
        Err(Refusal::NotDeveloperId)
    }
}

/// What was found out about a candidate app (on the mounted image, then its staged copy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `codesign -dv` of it.
    pub info: CodeInfo,
    /// `codesign --verify --deep --strict -R <requirement>` passed (its text otherwise).
    pub signature: Result<(), String>,
    /// `spctl --assess --type execute -vv`.
    pub assessment: Assessment,
    /// `CFBundleIdentifier` of its Info.plist.
    pub bundle_id: Option<String>,
    /// `CFBundleShortVersionString` of its Info.plist.
    pub version: Option<String>,
}

/// Why a candidate was refused. Nothing is replaced then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The signature is broken, or does not satisfy our requirement.
    Signature(String),
    /// Another bundle ID (Info.plist or signature).
    BundleId(Option<String>),
    /// Not a Developer ID signature.
    NotDeveloperId,
    /// Another team.
    Team(Option<String>),
    /// Gatekeeper did not accept it as notarized.
    NotNotarized(Option<String>),
    /// No readable version.
    NoVersion,
    /// A pre-release version (`1.3.0-rc.1`): updates only ever install releases.
    PreRelease(String),
    /// Not newer than the running app.
    NotNewer(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::Signature(e) => write!(f, "its code signature is not valid or not ours: {e}"),
            Rejection::BundleId(id) => write!(f, "it is another app ({id:?})"),
            Rejection::NotDeveloperId => write!(f, "it is not signed with a Developer ID"),
            Rejection::Team(t) => write!(f, "it is signed by another developer team ({t:?})"),
            Rejection::NotNotarized(s) => write!(f, "Gatekeeper did not accept it as notarized ({s:?})"),
            Rejection::NoVersion => write!(f, "it has no version"),
            Rejection::PreRelease(v) => write!(f, "its version {v} is a pre-release"),
            Rejection::NotNewer(v) => write!(f, "its version {v} is not newer than this one"),
        }
    }
}

/// Whether two version texts name the same version, compared parsed (`1.2` and `1.2.0` are
/// one version; build metadata is ignored). Text that is no version never matches.
pub fn same_version(a: &str, b: &str) -> bool {
    matches!((Version::parse(a), Version::parse(b)), (Some(x), Some(y)) if x.cmp(&y) == std::cmp::Ordering::Equal)
}

/// Whether `c` may replace the running app (version `current`) under `trust`.
pub fn judge(trust: &Trust, c: &Candidate, current: &Version) -> Result<Version, Rejection> {
    c.signature.clone().map_err(Rejection::Signature)?;
    if c.bundle_id.as_deref() != Some(trust.bundle_id.as_str()) {
        return Err(Rejection::BundleId(c.bundle_id.clone()));
    }
    if c.info.identifier.as_deref() != Some(trust.bundle_id.as_str()) {
        return Err(Rejection::BundleId(c.info.identifier.clone()));
    }
    if trust.strict {
        if !is_developer_id(&c.info) {
            return Err(Rejection::NotDeveloperId);
        }
        if c.info.team_id != trust.team_id {
            return Err(Rejection::Team(c.info.team_id.clone()));
        }
        if !notarized(&c.assessment) {
            return Err(Rejection::NotNotarized(c.assessment.source.clone()));
        }
    }
    let text = c.version.as_deref().ok_or(Rejection::NoVersion)?;
    let version = Version::parse(text).ok_or(Rejection::NoVersion)?;
    if version.is_pre_release() {
        return Err(Rejection::PreRelease(version.to_string()));
    }
    if version <= *current {
        return Err(Rejection::NotNewer(version.to_string()));
    }
    Ok(version)
}

// ---------------------------------------------------------------------------------------
// When

/// An automatic install waits until nothing came from the keyboard, mouse or trackpad for
/// this long (the user is away; the windows coming back and the relaunch are not in their way).
pub const AUTO_IDLE_SECS: f64 = 120.0;

/// What the moment looks like when an install is ready.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moment {
    /// A mouse button is down (a window or card drag may be under way).
    pub buttons_down: bool,
    /// The strip is mid-transition or a card is dragged.
    pub strip_busy: bool,
    /// The screen is locked (or the login session is not on the console): Accessibility
    /// cannot move a window then (`-25202`), so the quit could not bring parked ones back.
    pub locked: bool,
    /// The main display sleeps.
    pub asleep: bool,
    /// Seconds since the last input event, counted from the last lock or display sleep at
    /// most ([`idle_after_rest`]).
    pub idle_secs: f64,
}

/// Why a ready install does not go now ([`wait_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// A mouse button is down, or the strip is mid-transition.
    Busy,
    /// The screen is locked (an automatic install only).
    Locked,
    /// The display sleeps (an automatic install only).
    Asleep,
    /// The user has not been away [`AUTO_IDLE_SECS`] (an automatic install only).
    NotAway,
}

impl Wait {
    /// Its words for the log.
    pub fn name(self) -> &'static str {
        match self {
            Wait::Busy => "a drag or a transition is under way",
            Wait::Locked => "the screen is locked",
            Wait::Asleep => "the display sleeps",
            Wait::NotAway => "the user is not away",
        }
    }

    /// The screen is locked or the display sleeps: logged once per ready update.
    pub fn is_rest(self) -> bool {
        matches!(self, Wait::Locked | Wait::Asleep)
    }
}

/// Why a ready install may not quit the app now, if it may not: never mid-drag or
/// mid-transition; an automatic one (not asked for by a click) never while the screen is
/// locked or the display sleeps (a quit then cannot bring the parked windows back), and only
/// once the user has been away [`AUTO_IDLE_SECS`].
pub fn wait_reason(asked: bool, m: Moment) -> Option<Wait> {
    if m.buttons_down || m.strip_busy {
        Some(Wait::Busy)
    } else if asked {
        None
    } else if m.locked {
        Some(Wait::Locked)
    } else if m.asleep {
        Some(Wait::Asleep)
    } else if m.idle_secs < AUTO_IDLE_SECS {
        Some(Wait::NotAway)
    } else {
        None
    }
}

/// Whether a ready install may quit the app now ([`wait_reason`] gives none).
pub fn may_install_now(asked: bool, m: Moment) -> bool {
    wait_reason(asked, m).is_none()
}

/// The idle time an automatic install reads: the seconds since the last input event
/// (`input_idle`), but never more than the seconds since the screen was last seen locked or
/// the display asleep (`since_rest`). The input clock may still count from before the lock,
/// and the user who just unlocked is back at the desk.
pub fn idle_after_rest(input_idle: f64, since_rest: Option<f64>) -> f64 {
    since_rest.map_or(input_idle, |r| input_idle.min(r.max(0.0)))
}

/// What a quit looks like when it may install a ready update ([`quit_installs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuitFacts {
    /// The user chose Quit: the menu bar menu's item, or ⌘Q in Settings and About.
    pub user_quit: bool,
    /// Every window came back (the clean-exit marker is written; the guardian has no work).
    pub clean: bool,
    /// A logout, restart or shutdown: the system's quit said so, or the quit came during the
    /// logout's head start.
    pub for_logout: bool,
    /// Apple's Stage Manager is on (the app stands down).
    pub apple_on: bool,
    /// An installer helper is armed already: the quit was made for an install.
    pub helper_armed: bool,
}

/// Whether a quit installs the ready update (the owner's go 11-09): only the user's own, clean Quit;
/// never at a logout, restart or shutdown, while Apple's Stage Manager is on, after windows
/// were left for the guardian, or on a quit already made for an install (whose helper
/// reopens the app). Such an install does not reopen the app ([`HelperArgs::relaunch`]).
pub fn quit_installs(f: QuitFacts) -> bool {
    f.user_quit && f.clean && !f.for_logout && !f.apple_on && !f.helper_armed
}

/// What the launch found beside the installed app ([`checks_early`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleWork {
    /// No working folder.
    Nothing,
    /// An installer helper runs: the folder is its own.
    HelperRuns,
    /// A folder marked [`KEEP_MARK`]: it holds a copy an update could not put back.
    Kept,
    /// An earlier run's staged update, removed.
    Removed,
    /// An earlier run's staged update that could not be removed.
    NotRemoved,
}

/// Whether the launch checks for updates within its first minute instead of waiting for the
/// daily check: an earlier run downloaded and verified an update that never went in (it quit
/// while the update waited). The new check verifies from scratch; the old folder is never
/// trusted (a fresh download replaces it).
pub fn checks_early(found: StaleWork) -> bool {
    matches!(found, StaleWork::Removed | StaleWork::NotRemoved)
}

/// Whether an automatic check runs now: a left-behind update is owed one (`left_behind`, until
/// a check gets an answer), else when the daily check is due.
pub fn check_due(left_behind: bool, daily_due: bool) -> bool {
    left_behind || daily_due
}

// ---------------------------------------------------------------------------------------
// The helper's arguments

/// What the helper process (`stage-manager --update-helper …`) is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    /// The old app's process: the helper waits for it to exit.
    pub pid: u32,
    /// The installed app (replaced).
    pub target: PathBuf,
    /// The verified copy (moved in).
    pub staged: PathBuf,
    /// The version the new app must answer with.
    pub version: String,
    /// The `codesign -R` requirement, checked once more right before the swap.
    pub requirement: String,
    /// Whether the new app is opened after the swap: an install at a quiet moment reopens
    /// it; one at the user's own Quit does not (they asked it to stop), and nothing is
    /// opened after a failure either ([`run_swap`]).
    pub relaunch: bool,
}

/// The helper's last word when the app is reopened after the swap.
pub const RELAUNCH_WORD: &str = "relaunch";
/// The helper's last word when the app stays closed (an install at the user's Quit).
pub const NO_RELAUNCH_WORD: &str = "no-relaunch";

impl HelperArgs {
    /// The command-line words after `--update-helper`.
    pub fn to_args(&self) -> Vec<String> {
        vec![
            self.pid.to_string(),
            self.target.to_string_lossy().into_owned(),
            self.staged.to_string_lossy().into_owned(),
            self.version.clone(),
            self.requirement.clone(),
            if self.relaunch { RELAUNCH_WORD } else { NO_RELAUNCH_WORD }.to_string(),
        ]
    }

    /// Parses the words after `--update-helper`; the staged copy must sit in the working
    /// folder beside the target, both named [`APP_NAME`]. The helper is always the running
    /// app's own binary, so the words are always this version's: no older form is read.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let [pid, target, staged, version, requirement, relaunch] = args else {
            return Err(format!("expected 6 arguments, got {}", args.len()));
        };
        let relaunch = match relaunch.as_str() {
            RELAUNCH_WORD => true,
            NO_RELAUNCH_WORD => false,
            _ => return Err("bad relaunch word".into()),
        };
        let pid = pid.parse::<u32>().ok().filter(|p| *p > 1).ok_or("bad pid")?;
        let (target, staged) = (PathBuf::from(target), PathBuf::from(staged));
        if !target.is_absolute() || staging_paths(&target).map(|(_, s)| s) != Some(staged.clone()) {
            return Err("the staged copy is not beside the target".into());
        }
        if target.file_name().is_none_or(|n| n != APP_NAME) {
            return Err("the target is not Stage Manager.app".into());
        }
        if Version::parse(version).is_none_or(|v| v.is_pre_release()) {
            return Err("bad version".into());
        }
        if requirement.is_empty() {
            return Err("no requirement".into());
        }
        Ok(Self { pid, target, staged, version: version.clone(), requirement: requirement.clone(), relaunch })
    }
}

/// The environment the reopened app gets (`open --env`): the app's own variables
/// (`STAGE_MANAGER_*`, `RUST_LOG`), so a test run's reopened app stays a test run (its home,
/// its fixture apps only) and answers on the same ctl socket.
pub fn relaunch_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<String> {
    let mut out: Vec<String> = vars
        .into_iter()
        .filter(|(k, _)| k.starts_with("STAGE_MANAGER_") || k == "RUST_LOG")
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------
// The swap

/// The swap's steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The staged copy still satisfies the requirement (checked again right before).
    Verify,
    /// The staged copy and the installed app trade places, atomically.
    Exchange,
    /// The new app is opened (a relaunch only).
    Launch,
    /// The new app answers, with the new version; with no relaunch, the copy now at the
    /// installed path satisfies the requirement and carries the new version.
    Confirm,
    /// The old app (now at the staged path) is removed.
    RemoveOld,
}

/// The file and process operations the swap needs; the helper's are real, the tests' fake.
pub trait SwapOps {
    /// Checks the staged copy's signature against the requirement.
    fn verify_staged(&mut self) -> Result<(), String>;
    /// Exchanges the staged copy and the installed app (each takes the other's path). Either
    /// both moved, or neither did, or the error says where each one is.
    fn exchange(&mut self) -> Result<(), ExchangeError>;
    /// Opens the app at the installed path.
    fn launch(&mut self) -> Result<(), String>;
    /// Waits for the app just opened to answer with the expected version.
    fn confirm(&mut self) -> Result<(), String>;
    /// With no relaunch: checks the copy now at the installed path (its signature against the
    /// requirement, its version), in place of opening it.
    fn check_installed(&mut self) -> Result<(), String>;
    /// Stops the app the helper opened from the installed path that did not confirm (its quit
    /// path first, then a kill). Returns whether it is gone: `false` when it still runs.
    fn stop_launched(&mut self) -> bool;
    /// Removes the old app (at the staged path after the exchange) and the working folder.
    fn remove_old(&mut self) -> Result<(), String>;
}

/// How a swap ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    /// The new app runs; the old one is gone.
    Installed,
    /// The new app runs; the old one could not be removed (left in the working folder).
    InstalledOldLeft(String),
    /// A step failed; the old app is back in place and was opened again (`relaunched`; `Ok`
    /// with nothing opened when no relaunch was asked).
    RolledBack { step: Step, why: String, relaunched: Result<(), String> },
    /// A step failed and the old app could not be put back: the new one stays and is opened
    /// (on a relaunch); the old one is left at the staged path (the working folder is kept).
    RollbackFailed { step: Step, why: String, undo: String },
    /// A step failed and an exchange (the swap's, or the undo's) stopped part way: the old app
    /// is not at the installed path. `old` and `new` say where each one is; the working folder
    /// is kept. Whatever is at the installed path, if anything, was opened (`relaunched`).
    Stranded { step: Step, why: String, old: Place, new: Place, relaunched: Result<(), String> },
}

impl SwapOutcome {
    /// Whether the working folder must stay: it may hold the only copy of the old app (review
    /// R1). Only an installed new app, or the old one back in place, lets it go.
    pub fn keeps_work(&self) -> bool {
        matches!(self, Self::RollbackFailed { .. } | Self::Stranded { .. })
    }
}

/// Where one bundle is after an exchange stopped part way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The installed app's path.
    Target,
    /// The staged copy's path (in the working folder).
    Staged,
    /// The aside path of the three-rename exchange ([`ASIDE_NAME`], in the working folder).
    Aside,
}

impl Place {
    /// The path this place names, for an update from `staged` onto `target`.
    pub fn path(self, target: &Path, staged: &Path) -> PathBuf {
        match self {
            Self::Target => target.to_path_buf(),
            Self::Staged => staged.to_path_buf(),
            Self::Aside => staged.with_file_name(ASIDE_NAME),
        }
    }
}

/// Why an exchange failed, and where the two bundles are after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    /// Nothing moved: each bundle is still at its own path.
    Unchanged(String),
    /// Part of the exchange happened and its undo failed: where the bundle that was at the
    /// installed path (`was_target`) and the one that was at the staged path are now.
    Stranded { why: String, was_target: Place, was_staged: Place },
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unchanged(why) | Self::Stranded { why, .. } => f.write_str(why),
        }
    }
}

/// What [`SwapOutcome::RollbackFailed`]'s `undo` says when the unconfirmed new app would not
/// stop: the old app is not put back under a running one.
pub const NEW_APP_STILL_RUNS: &str = "the new version did not stop, so the old one was not put back under it";

/// Runs the swap: verify, exchange, launch, confirm, then remove the old app. A failure at
/// any step before the old app is removed puts it back (undoing the exchange) and opens it;
/// except when the unconfirmed new app cannot be stopped: then nothing is undone under it
/// (review 8C #2). An exchange that stops part way is reported where each app is, never as
/// rolled back unless the old app is at the installed path (review R1).
///
/// With no `relaunch` (an install at the user's own Quit) nothing is ever opened: the copy at
/// the installed path is checked instead of launched and confirmed ([`SwapOps::check_installed`]),
/// and a failure puts the old app back without opening it.
pub fn run_swap(ops: &mut dyn SwapOps, relaunch: bool) -> SwapOutcome {
    if let Err(why) = ops.verify_staged() {
        return SwapOutcome::RolledBack { step: Step::Verify, why, relaunched: reopen(ops, relaunch) };
    }
    match ops.exchange() {
        Ok(()) => {}
        Err(ExchangeError::Unchanged(why)) => {
            return SwapOutcome::RolledBack { step: Step::Exchange, why, relaunched: reopen(ops, relaunch) };
        }
        Err(ExchangeError::Stranded { why, was_target, was_staged }) => {
            return stranded(ops, relaunch, Step::Exchange, why, was_target, was_staged);
        }
    }
    let failed = if !relaunch {
        ops.check_installed().err().map(|why| (Step::Confirm, why))
    } else {
        match ops.launch() {
            Err(why) => Some((Step::Launch, why)),
            Ok(()) => match ops.confirm() {
                Ok(()) => None,
                Err(why) => {
                    if !ops.stop_launched() {
                        return SwapOutcome::RollbackFailed { step: Step::Confirm, why, undo: NEW_APP_STILL_RUNS.into() };
                    }
                    Some((Step::Confirm, why))
                }
            },
        }
    };
    if let Some((step, why)) = failed {
        return match ops.exchange() {
            Ok(()) => SwapOutcome::RolledBack { step, why, relaunched: reopen(ops, relaunch) },
            Err(ExchangeError::Unchanged(undo)) => {
                // The new app stays in place: open it rather than leave no app running (on a
                // relaunch; after the user's Quit, none was running).
                let _ = reopen(ops, relaunch);
                SwapOutcome::RollbackFailed { step, why, undo }
            }
            // The undo's exchange runs with the new app installed: its `was_target` is the new.
            Err(ExchangeError::Stranded { why: undo, was_target, was_staged }) => {
                stranded(ops, relaunch, step, format!("{why}; and putting the old version back: {undo}"), was_staged, was_target)
            }
        };
    }
    match ops.remove_old() {
        Ok(()) => SwapOutcome::Installed,
        Err(e) => SwapOutcome::InstalledOldLeft(e),
    }
}

/// Opens whatever is at the installed path after a failure, on a relaunch only; with none
/// asked, nothing is opened (`Ok`).
fn reopen(ops: &mut dyn SwapOps, relaunch: bool) -> Result<(), String> {
    if relaunch { ops.launch() } else { Ok(()) }
}

/// The outcome of an exchange that stopped part way with the old app at `old` and the new at
/// `new`: rolled back when the old app is at the installed path anyway, else stranded. Either
/// way, whatever is at the installed path is opened (on a relaunch).
fn stranded(ops: &mut dyn SwapOps, relaunch: bool, step: Step, why: String, old: Place, new: Place) -> SwapOutcome {
    let relaunched = reopen(ops, relaunch);
    if old == Place::Target {
        SwapOutcome::RolledBack { step, why, relaunched }
    } else {
        SwapOutcome::Stranded { step, why, old, new, relaunched }
    }
}

/// An exchange of `a` and `b` by three renames through `aside`, for volumes without an atomic
/// swap: `a → aside`, `b → a`, `aside → b`. A failure undoes the renames already made, so
/// either both moved or neither did; when an undo fails too, the error says where each bundle
/// is ([`Place::Target`] is `a`, [`Place::Staged`] is `b`).
pub fn exchange_by_renames(
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    a: &Path,
    b: &Path,
    aside: &Path,
) -> Result<(), ExchangeError> {
    use ExchangeError::{Stranded, Unchanged};
    rename(a, aside).map_err(|e| Unchanged(format!("cannot move {} aside: {e}", a.display())))?;
    if let Err(e) = rename(b, a) {
        return Err(match rename(aside, a) {
            Ok(()) => Unchanged(format!("cannot move {} in: {e}", b.display())),
            Err(u) => Stranded {
                why: format!("cannot move {} in: {e}; and cannot put {} back: {u}", b.display(), a.display()),
                was_target: Place::Aside,
                was_staged: Place::Staged,
            },
        });
    }
    if let Err(e) = rename(aside, b) {
        let why = |u| format!("cannot finish the exchange: {e}; and cannot undo it: {u}");
        if let Err(u) = rename(a, b) {
            return Err(Stranded { why: why(u), was_target: Place::Aside, was_staged: Place::Target });
        }
        return Err(match rename(aside, a) {
            Ok(()) => Unchanged(format!("cannot finish the exchange: {e}")),
            Err(u) => Stranded { why: why(u), was_target: Place::Aside, was_staged: Place::Staged },
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Processes

/// A process's arguments from `sysctl(KERN_PROCARGS2)`'s buffer: `argc` (a native-endian
/// `i32`), the executable path, NUL padding, then `argc` NUL-terminated words (the
/// environment follows; it is not read). `None` when the buffer is short or malformed.
pub fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok().filter(|&n| n > 0 && n < 4096)?;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let mut tail = &rest[path_end..];
    tail = &tail[tail.iter().position(|&b| b != 0)?..];
    let mut out = Vec::with_capacity(argc);
    for _ in 0..argc {
        // Every word ends in a NUL; a buffer that stops short is malformed.
        let end = tail.iter().position(|&b| b == 0)?;
        out.push(String::from_utf8_lossy(&tail[..end]).into_owned());
        tail = &tail[end + 1..];
    }
    Some(out)
}

/// Whether a process running the app's executable with these arguments is the app itself:
/// no subcommand (`ctl`, `--guardian`, `--update-helper`, …), at most the `-psn_…` word older
/// LaunchServices adds. The user's CLI and the app's own helpers are never the app.
pub fn is_app_instance(argv: &[String]) -> bool {
    !argv.is_empty() && argv[1..].iter().all(|a| a.starts_with("-psn_"))
}

/// Whether these arguments are the update helper's (`<exe> --update-helper …`).
pub fn is_update_helper(argv: &[String]) -> bool {
    argv.get(1).is_some_and(|a| a == "--update-helper")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DEV_ID: &str = "Executable=/Applications/Stage Manager.app/Contents/MacOS/stage-manager
Identifier=dev.eugene.stagemanager
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20500 size=643 flags=0x10000(runtime) hashes=9+7 location=embedded
Signature size=9048
Authority=Developer ID Application: Some Person (AB12CD34EF)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
Timestamp=Oct 13, 2026 at 7:00:00 PM
TeamIdentifier=AB12CD34EF
Runtime Version=26.0.0";

    const OWN_DR: &str = "anchor apple generic and identifier \"dev.eugene.stagemanager\" and (certificate leaf[field.1.2.840.113635.100.6.1.9] /* exists */ or certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \"AB12CD34EF\")";

    const BUNDLE: &str = "dev.eugene.stagemanager";

    fn v(s: &str) -> Version {
        Version::parse(s).expect("version")
    }

    fn info(text: &str) -> CodeInfo {
        parse_codesign_display(text)
    }

    fn trust() -> Trust {
        Trust::from_own(&info(DEV_ID), Some(OWN_DR), BUNDLE, false).expect("trusted")
    }

    fn good() -> Candidate {
        Candidate {
            info: info(DEV_ID),
            signature: Ok(()),
            assessment: parse_spctl("/x/Stage Manager.app: accepted\nsource=Notarized Developer ID\norigin=Developer ID Application: Some Person (AB12CD34EF)\n", true),
            bundle_id: Some(BUNDLE.into()),
            version: Some("1.2.0".into()),
        }
    }

    #[test]
    fn codesign_and_spctl_output_parse() {
        let i = info(DEV_ID);
        assert_eq!(i.identifier.as_deref(), Some(BUNDLE));
        assert_eq!(i.team_id.as_deref(), Some("AB12CD34EF"));
        assert_eq!(i.authorities.len(), 3);
        assert!(is_developer_id(&i));
        let local = info("Identifier=dev.eugene.stagemanager\nAuthority=Stage Manager Local Signing\nTeamIdentifier=not set\n");
        assert_eq!(local.team_id, None);
        assert!(!is_developer_id(&local));
        let adhoc = info("Identifier=stage-manager\nSignature=adhoc\nTeamIdentifier=not set\n");
        assert!(adhoc.adhoc && !is_developer_id(&adhoc));
        assert_eq!(parse_designated(&format!("Executable=/x\ndesignated => {OWN_DR}\n")).as_deref(), Some(OWN_DR));
        assert_eq!(parse_designated("Executable=/x\n"), None);
        let rejected = parse_spctl("/x: rejected\nsource=Unnotarized Developer ID\n", false);
        assert!(!rejected.accepted && !notarized(&rejected));
        let devid_only = parse_spctl("/x: accepted\nsource=Developer ID\n", true);
        assert!(devid_only.accepted && !notarized(&devid_only), "accepted, but not as notarized");
        assert!(!notarized(&parse_spctl("/x: accepted\nsource=Notarized Developer ID\n", false)), "a non-zero exit is never accepted");
    }

    #[test]
    fn trust_comes_from_the_running_apps_own_signature() {
        let t = trust();
        assert_eq!(t.team_id.as_deref(), Some("AB12CD34EF"));
        assert!(t.strict);
        assert!(t.requirement.starts_with(&format!("({OWN_DR}) and (")), "the running app's designated requirement first");
        assert!(t.requirement.contains("certificate leaf[subject.OU] = \"AB12CD34EF\""));
        assert!(t.requirement.contains("identifier \"dev.eugene.stagemanager\""));
        let local = info("Identifier=dev.eugene.stagemanager\nAuthority=Stage Manager Local Signing\nTeamIdentifier=not set\n");
        assert_eq!(Trust::from_own(&local, Some("identifier x"), BUNDLE, false), Err(Refusal::NotDeveloperId), "a local build never self-updates");
        let relaxed = Trust::from_own(&local, Some("identifier x"), BUNDLE, true).expect("test builds only");
        assert!(!relaxed.strict && relaxed.requirement == "identifier x");
        let adhoc = info("Identifier=dev.eugene.stagemanager\nSignature=adhoc\n");
        assert_eq!(Trust::from_own(&adhoc, Some("cdhash H\"00\""), BUNDLE, true), Err(Refusal::NotDeveloperId), "never ad hoc");
        assert!(matches!(Trust::from_own(&info(DEV_ID), None, BUNDLE, false), Err(Refusal::NoOwnSignature(_))));
        assert!(matches!(Trust::from_own(&info(DEV_ID), Some(OWN_DR), "com.other", false), Err(Refusal::NoOwnSignature(_))));
        assert!(matches!(Trust::from_own(&info(DEV_ID), Some("a\nb"), BUNDLE, false), Err(Refusal::NoOwnSignature(_))));
    }

    #[test]
    fn a_candidate_must_prove_every_point() {
        let t = trust();
        let now = v("1.1.0");
        assert_eq!(judge(&t, &good(), &now), Ok(v("1.2.0")));

        let mut c = good();
        c.signature = Err("a sealed resource is missing or invalid".into());
        assert!(matches!(judge(&t, &c, &now), Err(Rejection::Signature(_))));

        let mut c = good();
        c.bundle_id = Some("com.evil.app".into());
        assert!(matches!(judge(&t, &c, &now), Err(Rejection::BundleId(_))));

        let mut c = good();
        c.info.identifier = Some("com.evil.app".into());
        assert!(matches!(judge(&t, &c, &now), Err(Rejection::BundleId(_))));

        let mut c = good();
        c.info.authorities[0] = "Apple Development: Someone (XYZ)".into();
        assert_eq!(judge(&t, &c, &now), Err(Rejection::NotDeveloperId));

        let mut c = good();
        c.info.team_id = Some("ZZZZZZZZZZ".into());
        assert_eq!(judge(&t, &c, &now), Err(Rejection::Team(Some("ZZZZZZZZZZ".into()))));

        let mut c = good();
        c.assessment = parse_spctl("/x: accepted\nsource=Developer ID\n", true);
        assert!(matches!(judge(&t, &c, &now), Err(Rejection::NotNotarized(_))));

        let mut c = good();
        c.version = None;
        assert_eq!(judge(&t, &c, &now), Err(Rejection::NoVersion));

        for older in ["1.1.0", "1.0.9", "1.1"] {
            let mut c = good();
            c.version = Some(older.into());
            assert!(matches!(judge(&t, &c, &now), Err(Rejection::NotNewer(_))), "{older}");
        }

        // Review 8C #3: a pre-release never installs, even one newer than this version.
        for pre in ["1.2.0-rc.1", "2.0.0-beta", "1.1.0-rc.1"] {
            let mut c = good();
            c.version = Some(pre.into());
            assert!(matches!(judge(&t, &c, &now), Err(Rejection::PreRelease(_))), "{pre}");
        }
    }

    /// Review 8C #4: the staged copy's and the new app's versions are compared parsed.
    #[test]
    fn versions_match_parsed_not_as_text() {
        assert!(same_version("1.2.0", "1.2.0"));
        assert!(same_version("1.2", "1.2.0"), "Info.plist may leave out the patch");
        assert!(same_version("v1.2.0", "1.2.0+build.3"));
        assert!(!same_version("1.2.0", "1.2.1"));
        assert!(!same_version("1.2.0", "1.2.0-rc.1"));
        assert!(!same_version("1.2.0", "garbage") && !same_version("", ""));
    }

    /// Review 8C #7: only the app itself is stopped, never the user's CLI or its helpers.
    #[test]
    fn process_arguments_parse_and_only_the_app_is_the_app() {
        let words = |argv: &[&str]| argv.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let buf = |argc: i32, path: &str, argv: &[&str]| {
            let mut b = argc.to_ne_bytes().to_vec();
            b.extend_from_slice(path.as_bytes());
            b.extend_from_slice(&[0, 0, 0]);
            for a in argv {
                b.extend_from_slice(a.as_bytes());
                b.push(0);
            }
            b.extend_from_slice(b"HOME=/Users/me\0");
            b
        };
        let exe = "/Applications/Stage Manager.app/Contents/MacOS/stage-manager";
        assert_eq!(parse_procargs2(&buf(1, exe, &[exe])), Some(words(&[exe])));
        assert_eq!(parse_procargs2(&buf(3, exe, &[exe, "ctl", "ping"])), Some(words(&[exe, "ctl", "ping"])));
        assert_eq!(parse_procargs2(&buf(3, exe, &[exe])), None, "fewer words than argc");
        assert_eq!(parse_procargs2(&buf(0, exe, &[])), None);
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert!(is_app_instance(&words(&[exe])));
        assert!(is_app_instance(&words(&[exe, "-psn_0_12345"])));
        for other in [&[exe, "ctl", "ping"][..], &[exe, "--guardian", "42"], &[exe, "--update-helper", "1"], &[exe, "log", "path"], &[]] {
            assert!(!is_app_instance(&words(other)), "{other:?}");
        }
        assert!(is_update_helper(&words(&[exe, "--update-helper", "42"])));
        assert!(!is_update_helper(&words(&[exe])) && !is_update_helper(&words(&[exe, "ctl", "--update-helper"])));
    }

    #[test]
    fn only_a_writable_installed_bundle_is_replaced() {
        let ok = Path::new("/Applications/Stage Manager.app");
        assert_eq!(install_target(ok, true), Ok(ok.to_path_buf()));
        assert_eq!(install_target(Path::new("/Users/me/Applications/Stage Manager.app"), true).map(|_| ()), Ok(()));
        assert_eq!(install_target(ok, false), Err(Refusal::NotWritable(PathBuf::from("/Applications"))));
        assert!(install_target(ok, false).unwrap_err().opens_disk_image());
        assert_eq!(install_target(Path::new("/Volumes/Stage Manager/Stage Manager.app"), true), Err(Refusal::OnDiskImage));
        assert_eq!(
            install_target(Path::new("/private/var/folders/x/T/AppTranslocation/1234/d/Stage Manager.app"), true),
            Err(Refusal::Translocated)
        );
        assert_eq!(install_target(Path::new("/Users/me/src/target/release"), true), Err(Refusal::NotABundle));
        assert_eq!(install_target(Path::new("/Applications/Other.app"), true), Err(Refusal::NotABundle), "only our own bundle name");
        for r in [Refusal::NotABundle, Refusal::Translocated, Refusal::NotWritable(PathBuf::from("/Applications")), Refusal::NotDeveloperId] {
            assert!(r.sentence().ends_with('.'), "{r:?}");
        }
        let (work, staged) = staging_paths(ok).expect("paths");
        assert_eq!(work, PathBuf::from("/Applications/.Stage Manager update"));
        assert_eq!(staged, PathBuf::from("/Applications/.Stage Manager update/Stage Manager.app"));
    }

    #[test]
    fn never_mid_drag_or_mid_transition_and_automatic_only_when_away() {
        let calm = Moment { buttons_down: false, strip_busy: false, locked: false, asleep: false, idle_secs: 0.5 };
        assert!(may_install_now(true, calm), "a click on Install and Relaunch goes at once");
        assert!(!may_install_now(false, calm), "an automatic install waits for the user to be away");
        assert_eq!(wait_reason(false, calm), Some(Wait::NotAway));
        let away = Moment { idle_secs: AUTO_IDLE_SECS, ..calm };
        assert!(may_install_now(false, away));
        for asked in [true, false] {
            assert!(!may_install_now(asked, Moment { buttons_down: true, ..away }), "never mid-drag");
            assert!(!may_install_now(asked, Moment { strip_busy: true, ..away }), "never mid-transition");
        }
    }

    #[test]
    fn an_automatic_install_never_goes_while_the_screen_is_locked_or_the_display_sleeps() {
        // The owner's log, 10-30: locked at 08:31, 38 min idle at 08:59, the install quit and every
        // parked window failed to come back (AX -25202) until the unlock.
        let away = Moment { buttons_down: false, strip_busy: false, locked: false, asleep: false, idle_secs: 2294.0 };
        assert_eq!(wait_reason(false, Moment { locked: true, ..away }), Some(Wait::Locked));
        assert_eq!(wait_reason(false, Moment { asleep: true, ..away }), Some(Wait::Asleep));
        assert_eq!(wait_reason(false, Moment { locked: true, asleep: true, ..away }), Some(Wait::Locked));
        assert!(Wait::Locked.is_rest() && Wait::Asleep.is_rest() && !Wait::NotAway.is_rest() && !Wait::Busy.is_rest());
        // A click-asked install is unchanged.
        assert!(may_install_now(true, Moment { locked: true, asleep: true, idle_secs: 0.0, ..away }));
        assert!(may_install_now(false, away));
    }

    #[test]
    fn after_an_unlock_the_idle_time_counts_from_the_unlock() {
        assert_eq!(idle_after_rest(2294.0, None), 2294.0, "never locked: the input clock");
        assert_eq!(idle_after_rest(2294.0, Some(3.0)), 3.0, "unlocked 3 s ago: back at the desk");
        assert_eq!(idle_after_rest(10.0, Some(600.0)), 10.0, "input since the unlock counts");
        assert_eq!(idle_after_rest(2294.0, Some(-1.0)), 0.0);
        let back = Moment { buttons_down: false, strip_busy: false, locked: false, asleep: false, idle_secs: idle_after_rest(2294.0, Some(5.0)) };
        assert_eq!(wait_reason(false, back), Some(Wait::NotAway));
        let later = Moment { idle_secs: idle_after_rest(2294.0 + AUTO_IDLE_SECS, Some(AUTO_IDLE_SECS)), ..back };
        assert!(may_install_now(false, later), "away {AUTO_IDLE_SECS} s after the unlock: it goes");
    }

    #[test]
    fn the_reopened_app_keeps_the_apps_own_environment_only() {
        let vars = [("STAGE_MANAGER_ONLY", "dev.fixture"), ("HOME", "/Users/me"), ("RUST_LOG", "debug"), ("STAGE_MANAGER_HOME", "/tmp/h"), ("PATH", "/bin")]
            .map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(relaunch_env(vars), vec!["RUST_LOG=debug", "STAGE_MANAGER_HOME=/tmp/h", "STAGE_MANAGER_ONLY=dev.fixture"]);
    }

    #[test]
    fn the_helpers_arguments_round_trip_and_are_checked() {
        let a = HelperArgs {
            pid: 4242,
            target: PathBuf::from("/Applications/Stage Manager.app"),
            staged: PathBuf::from("/Applications/.Stage Manager update/Stage Manager.app"),
            version: "1.2.0".into(),
            requirement: OWN_DR.into(),
            relaunch: true,
        };
        assert_eq!(HelperArgs::parse(&a.to_args()), Ok(a.clone()));
        assert_eq!(a.to_args()[5], "relaunch");
        let mut bad = a.to_args();
        bad[2] = "/tmp/Stage Manager.app".into();
        assert!(HelperArgs::parse(&bad).is_err(), "a staged copy elsewhere is refused");
        let mut bad = a.to_args();
        bad[1] = "/Applications/Other.app".into();
        assert!(HelperArgs::parse(&bad).is_err());
        let mut bad = a.to_args();
        bad[0] = "1".into();
        assert!(HelperArgs::parse(&bad).is_err());
        let mut bad = a.to_args();
        bad[3] = "1.2.0-rc.1".into();
        assert!(HelperArgs::parse(&bad).is_err(), "never a pre-release");
        assert!(HelperArgs::parse(&a.to_args()[..4]).is_err());
        assert!(HelperArgs::parse(&a.to_args()[..5]).is_err(), "the relaunch word is required");
        let mut bad = a.to_args();
        bad[5] = "yes".into();
        assert!(HelperArgs::parse(&bad).is_err());
    }

    /// The owner's go 11-09: an install at the user's Quit tells the helper not to reopen the app.
    #[test]
    fn a_quit_install_tells_the_helper_not_to_relaunch() {
        let a = HelperArgs {
            pid: 4242,
            target: PathBuf::from("/Applications/Stage Manager.app"),
            staged: PathBuf::from("/Applications/.Stage Manager update/Stage Manager.app"),
            version: "1.7.0".into(),
            requirement: OWN_DR.into(),
            relaunch: false,
        };
        let words = a.to_args();
        assert_eq!(words.len(), 6);
        assert_eq!(words[5], "no-relaunch");
        assert_eq!(HelperArgs::parse(&words).map(|p| p.relaunch), Ok(false));
    }

    /// The owner's go 11-09: only the user's own clean Quit installs a ready update.
    #[test]
    fn only_the_users_own_quit_installs() {
        let user = QuitFacts { user_quit: true, clean: true, for_logout: false, apple_on: false, helper_armed: false };
        assert!(quit_installs(user), "Quit in the menu, or ⌘Q in Settings and About");
        assert!(!quit_installs(QuitFacts { user_quit: false, ..user }), "the system's quit, ctl quit, a signal");
        assert!(!quit_installs(QuitFacts { for_logout: true, ..user }), "a logout, restart or shutdown");
        assert!(!quit_installs(QuitFacts { for_logout: true, user_quit: false, ..user }));
        assert!(!quit_installs(QuitFacts { apple_on: true, ..user }), "the stand-down");
        assert!(!quit_installs(QuitFacts { helper_armed: true, ..user }), "a quit already made for an install");
        assert!(!quit_installs(QuitFacts { clean: false, ..user }), "windows left for the guardian");
    }

    /// The owner's go 11-09: a staged update an earlier run left is checked again at once, from
    /// scratch; a kept copy (a failed install's) and a running helper's folder are not.
    #[test]
    fn a_left_behind_update_is_checked_again_early() {
        assert!(checks_early(StaleWork::Removed));
        assert!(checks_early(StaleWork::NotRemoved));
        for w in [StaleWork::Nothing, StaleWork::HelperRuns, StaleWork::Kept] {
            assert!(!checks_early(w), "{w:?}");
        }
        assert!(check_due(true, false), "owed a check: not waiting for the daily one");
        assert!(check_due(false, true));
        assert!(!check_due(false, false));
    }
    /// A fake disk and process table: which app sits at the target and the staged path, what
    /// runs, and one step that fails.
    struct Fake {
        target: Option<&'static str>,
        staged: Option<&'static str>,
        running: Option<&'static str>,
        launched: Vec<&'static str>,
        fail: Option<Step>,
        /// The exchange fails from its nth call on (the undo).
        fail_exchange_from: Option<usize>,
        exchanges: usize,
        confirm_new_only: bool,
        /// The unconfirmed new app ignores being stopped.
        unstoppable: bool,
    }

    impl Fake {
        fn new(fail: Option<Step>) -> Self {
            Self {
                target: Some("old"),
                staged: Some("new"),
                running: None,
                launched: Vec::new(),
                fail,
                fail_exchange_from: None,
                exchanges: 0,
                confirm_new_only: true,
                unstoppable: false,
            }
        }
    }

    impl SwapOps for Fake {
        fn verify_staged(&mut self) -> Result<(), String> {
            if self.fail == Some(Step::Verify) { Err("bad".into()) } else { Ok(()) }
        }
        fn exchange(&mut self) -> Result<(), ExchangeError> {
            self.exchanges += 1;
            let undo_fails = self.fail_exchange_from.is_some_and(|n| self.exchanges >= n);
            if (self.fail == Some(Step::Exchange) && self.exchanges == 1) || undo_fails {
                return Err(ExchangeError::Unchanged("no swap".into()));
            }
            std::mem::swap(&mut self.target, &mut self.staged);
            Ok(())
        }
        fn launch(&mut self) -> Result<(), String> {
            if self.fail == Some(Step::Launch) && self.target == Some("new") {
                return Err("open failed".into());
            }
            let app = self.target.ok_or("nothing there")?;
            self.launched.push(app);
            self.running = Some(app);
            Ok(())
        }
        fn confirm(&mut self) -> Result<(), String> {
            let ok = self.running == Some("new") || !self.confirm_new_only;
            if self.fail == Some(Step::Confirm) || !ok { Err("no answer".into()) } else { Ok(()) }
        }
        fn check_installed(&mut self) -> Result<(), String> {
            let ok = self.target == Some("new") && self.running.is_none();
            if self.fail == Some(Step::Confirm) || !ok { Err("does not verify".into()) } else { Ok(()) }
        }
        fn stop_launched(&mut self) -> bool {
            if !self.unstoppable {
                self.running = None;
            }
            self.running.is_none()
        }
        fn remove_old(&mut self) -> Result<(), String> {
            if self.fail == Some(Step::RemoveOld) {
                return Err("busy".into());
            }
            self.staged = None;
            Ok(())
        }
    }

    #[test]
    fn a_clean_swap_installs_and_removes_the_old_app_after_the_new_one_answers() {
        let mut f = Fake::new(None);
        assert_eq!(run_swap(&mut f, true), SwapOutcome::Installed);
        assert_eq!(f.target, Some("new"));
        assert_eq!(f.staged, None, "the old one is removed only at the end");
        assert_eq!(f.launched, vec!["new"]);
        assert_eq!(f.running, Some("new"));
    }

    /// The owner's go 11-09: an install at the user's Quit replaces the app and opens nothing; a
    /// failure puts the old one back, still without opening anything.
    #[test]
    fn a_quit_install_opens_nothing() {
        let mut f = Fake::new(None);
        assert_eq!(run_swap(&mut f, false), SwapOutcome::Installed);
        assert_eq!(f.target, Some("new"));
        assert_eq!(f.staged, None);
        assert!(f.launched.is_empty() && f.running.is_none(), "not reopened: {:?}", f.launched);
        for step in [Step::Verify, Step::Exchange, Step::Confirm] {
            let mut f = Fake::new(Some(step));
            let out = run_swap(&mut f, false);
            assert!(matches!(&out, SwapOutcome::RolledBack { step: s, relaunched: Ok(()), .. } if *s == step), "{step:?}: {out:?}");
            assert_eq!(f.target, Some("old"), "{step:?}: the old app is back");
            assert!(f.launched.is_empty(), "{step:?}: nothing opened");
        }
        let mut f = Fake::new(Some(Step::Confirm));
        f.fail_exchange_from = Some(2);
        let out = run_swap(&mut f, false);
        assert!(matches!(out, SwapOutcome::RollbackFailed { .. }), "{out:?}");
        assert!(f.launched.is_empty(), "the new app stays, unopened");
    }

    #[test]
    fn a_failure_at_each_step_puts_the_old_app_back_and_opens_it() {
        for step in [Step::Verify, Step::Exchange, Step::Launch, Step::Confirm] {
            let mut f = Fake::new(Some(step));
            let out = run_swap(&mut f, true);
            assert!(matches!(&out, SwapOutcome::RolledBack { step: s, relaunched: Ok(()), .. } if *s == step), "{step:?}: {out:?}");
            assert_eq!(f.target, Some("old"), "{step:?}: the old app is back in place");
            assert_eq!(f.staged, Some("new"), "{step:?}: the new copy is set aside, not lost");
            assert_eq!(f.running, Some("old"), "{step:?}: the old app runs again");
            assert_eq!(f.launched.last(), Some(&"old"));
        }
        let mut f = Fake::new(Some(Step::Confirm));
        run_swap(&mut f, true);
        assert_eq!(f.launched, vec!["new", "old"], "the unconfirmed new app was stopped before the old one opened");
    }

    #[test]
    fn a_failed_removal_keeps_the_new_app() {
        let mut f = Fake::new(Some(Step::RemoveOld));
        assert!(matches!(run_swap(&mut f, true), SwapOutcome::InstalledOldLeft(_)));
        assert_eq!((f.target, f.staged, f.running), (Some("new"), Some("old"), Some("new")));
    }

    #[test]
    fn a_failed_undo_still_leaves_an_app_running() {
        let mut f = Fake::new(Some(Step::Confirm));
        f.fail_exchange_from = Some(2);
        f.confirm_new_only = true;
        let out = run_swap(&mut f, true);
        assert!(matches!(out, SwapOutcome::RollbackFailed { step: Step::Confirm, .. }), "{out:?}");
        assert_eq!(f.running, Some("new"), "the new app is opened rather than none");
    }

    /// Review 8C #2: a new app that will not stop is not swapped out from under itself.
    #[test]
    fn an_unstoppable_new_app_is_left_in_place_and_said_so() {
        let mut f = Fake::new(Some(Step::Confirm));
        f.unstoppable = true;
        let out = run_swap(&mut f, true);
        assert!(matches!(&out, SwapOutcome::RollbackFailed { step: Step::Confirm, undo, .. } if undo == NEW_APP_STILL_RUNS), "{out:?}");
        assert_eq!(f.exchanges, 1, "no undo under a running app");
        assert_eq!((f.target, f.staged, f.running), (Some("new"), Some("old"), Some("new")));
        assert_eq!(f.launched, vec!["new"], "the old app is not opened beside it");
    }

    /// A fake folder for the three-rename exchange, failing at the nth rename.
    fn renames(fail_at: Option<usize>) -> (HashMap<PathBuf, &'static str>, Result<(), ExchangeError>) {
        let (a, b, aside) = (Path::new("/A"), Path::new("/B"), Path::new("/aside"));
        let mut fs: HashMap<PathBuf, &'static str> = HashMap::from([(a.to_path_buf(), "old"), (b.to_path_buf(), "new")]);
        let mut n = 0;
        let r = {
            let mut rename = |from: &Path, to: &Path| -> std::io::Result<()> {
                n += 1;
                if fail_at == Some(n) || fs.contains_key(to) {
                    return Err(std::io::Error::other("refused"));
                }
                let v = fs.remove(from).ok_or_else(|| std::io::Error::other("missing"))?;
                fs.insert(to.to_path_buf(), v);
                Ok(())
            };
            exchange_by_renames(&mut rename, a, b, aside)
        };
        (fs, r)
    }

    #[test]
    fn the_rename_exchange_is_all_or_nothing() {
        let (fs, r) = renames(None);
        assert_eq!(r, Ok(()));
        assert_eq!((fs[Path::new("/A")], fs[Path::new("/B")]), ("new", "old"));
        for fail in 1..=3 {
            let (fs, r) = renames(Some(fail));
            assert!(r.is_err(), "rename {fail}");
            assert_eq!(fs.get(Path::new("/A")), Some(&"old"), "rename {fail}: undone");
            assert_eq!(fs.get(Path::new("/B")), Some(&"new"), "rename {fail}: undone");
            assert_eq!(fs.len(), 2, "rename {fail}: nothing left aside");
        }
    }

    /// The bundle each place holds after [`exchange_by_renames`] failed at the given renames.
    fn renames_failing(fails: &[usize]) -> (HashMap<PathBuf, &'static str>, Result<(), ExchangeError>) {
        let (a, b, aside) = (Path::new("/A"), Path::new("/B"), Path::new("/aside"));
        let mut fs: HashMap<PathBuf, &'static str> = HashMap::from([(a.to_path_buf(), "old"), (b.to_path_buf(), "new")]);
        let mut n = 0;
        let r = exchange_by_renames(
            &mut |from: &Path, to: &Path| {
                n += 1;
                if fails.contains(&n) || fs.contains_key(to) {
                    return Err(std::io::Error::other("refused"));
                }
                let v = fs.remove(from).ok_or_else(|| std::io::Error::other("missing"))?;
                fs.insert(to.to_path_buf(), v);
                Ok(())
            },
            a,
            b,
            aside,
        );
        (fs, r)
    }

    /// Review R1: a failed undo says where each bundle is, and says it truly.
    #[test]
    fn a_failed_undo_says_where_each_bundle_is() {
        let at = |p: Place| PathBuf::from(match p {
            Place::Target => "/A",
            Place::Staged => "/B",
            Place::Aside => "/aside",
        });
        for mask in 0u32..32 {
            let fails: Vec<usize> = (1..=5).filter(|i| mask & (1 << (i - 1)) != 0).collect();
            let (fs, r) = renames_failing(&fails);
            assert_eq!(fs.len(), 2, "{fails:?}: both bundles survive");
            match r {
                Ok(()) => assert_eq!((fs[Path::new("/A")], fs[Path::new("/B")]), ("new", "old")),
                Err(ExchangeError::Unchanged(_)) => {
                    assert_eq!((fs[Path::new("/A")], fs[Path::new("/B")]), ("old", "new"), "{fails:?}")
                }
                Err(ExchangeError::Stranded { was_target, was_staged, .. }) => {
                    assert_eq!(fs.get(&at(was_target)), Some(&"old"), "{fails:?}");
                    assert_eq!(fs.get(&at(was_staged)), Some(&"new"), "{fails:?}");
                }
            }
        }
        let (_, r) = renames_failing(&[2, 3]);
        assert!(matches!(r, Err(ExchangeError::Stranded { was_target: Place::Aside, was_staged: Place::Staged, .. })), "{r:?}");
    }

    /// The swap over a real scratch folder, the three-rename exchange failing at chosen renames
    /// (the swap's and the undo's, and their compensating renames).
    struct Disk {
        target: PathBuf,
        staged: PathBuf,
        fails: Vec<usize>,
        renames: usize,
        fail: Option<Step>,
    }

    impl Disk {
        fn holds(path: &Path) -> Option<String> {
            std::fs::read_to_string(path.join("version")).ok()
        }
    }

    impl SwapOps for Disk {
        fn verify_staged(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn exchange(&mut self) -> Result<(), ExchangeError> {
            let aside = Place::Aside.path(&self.target, &self.staged);
            let (fails, n) = (&self.fails, &mut self.renames);
            exchange_by_renames(
                &mut |from, to| {
                    *n += 1;
                    if fails.contains(n) {
                        return Err(std::io::Error::other("injected"));
                    }
                    std::fs::rename(from, to)
                },
                &self.target,
                &self.staged,
                &aside,
            )
        }
        fn launch(&mut self) -> Result<(), String> {
            match Self::holds(&self.target).as_deref() {
                None => Err("nothing there".into()),
                Some("new") if self.fail == Some(Step::Launch) => Err("open failed".into()),
                Some(_) => Ok(()),
            }
        }
        fn confirm(&mut self) -> Result<(), String> {
            if self.fail == Some(Step::Confirm) { Err("no answer".into()) } else { Ok(()) }
        }
        fn check_installed(&mut self) -> Result<(), String> {
            match Self::holds(&self.target).as_deref() {
                Some("new") if self.fail != Some(Step::Confirm) => Ok(()),
                other => Err(format!("the installed path holds {other:?}")),
            }
        }
        fn stop_launched(&mut self) -> bool {
            true
        }
        fn remove_old(&mut self) -> Result<(), String> {
            std::fs::remove_dir_all(self.staged.parent().unwrap()).map_err(|e| e.to_string())
        }
    }

    /// Review R1: whichever renames fail, at least one whole app survives the helper's cleanup,
    /// and the outcome says where it is.
    #[test]
    fn a_stranded_exchange_never_loses_the_last_app() {
        let runs = [(None, true), (Some(Step::Launch), true), (Some(Step::Confirm), true), (None, false), (Some(Step::Confirm), false)];
        for (fail, relaunch) in runs {
            for mask in 0u32..(1 << 10) {
                let fails: Vec<usize> = (1..=10).filter(|i| mask & (1 << (i - 1)) != 0).collect();
                let tmp = tempfile::tempdir().unwrap();
                let work = tmp.path().join(WORK_DIR);
                let mut d = Disk { target: tmp.path().join(APP_NAME), staged: work.join(APP_NAME), fails: fails.clone(), renames: 0, fail };
                for (path, v) in [(&d.target, "old"), (&d.staged, "new")] {
                    std::fs::create_dir_all(path).unwrap();
                    std::fs::write(path.join("version"), v).unwrap();
                }
                let out = run_swap(&mut d, relaunch);
                // The helper's cleanup.
                if matches!(out, SwapOutcome::RolledBack { .. }) {
                    let _ = std::fs::remove_dir_all(&work);
                }
                assert_eq!(out.keeps_work(), work.exists(), "{fail:?} {fails:?}: {out:?}");
                let (target, staged) = (Disk::holds(&d.target), Disk::holds(&d.staged));
                let ctx = format!("{fail:?} relaunch {relaunch} {fails:?}: {out:?}");
                match &out {
                    SwapOutcome::Installed | SwapOutcome::InstalledOldLeft(_) => assert_eq!(target.as_deref(), Some("new"), "{ctx}"),
                    SwapOutcome::RolledBack { .. } => assert_eq!(target.as_deref(), Some("old"), "{ctx}"),
                    SwapOutcome::RollbackFailed { .. } => {
                        assert_eq!((target.as_deref(), staged.as_deref()), (Some("new"), Some("old")), "{ctx}")
                    }
                    SwapOutcome::Stranded { old, new, relaunched, .. } => {
                        assert_eq!(Disk::holds(&old.path(&d.target, &d.staged)).as_deref(), Some("old"), "{ctx}");
                        assert_eq!(Disk::holds(&new.path(&d.target, &d.staged)).as_deref(), Some("new"), "{ctx}");
                        assert_ne!(*old, Place::Target, "{ctx}");
                        assert!(*new == Place::Target || relaunched.is_err() || !relaunch, "{ctx}: nothing to open");
                    }
                }
            }
        }
    }

    /// Review R1's case: the fallback's second rename and its undo fail; the old app is only in
    /// the working folder, so it is not called rolled back and the folder stays.
    #[test]
    fn the_review_case_keeps_the_old_app() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join(WORK_DIR);
        let mut d = Disk { target: tmp.path().join(APP_NAME), staged: work.join(APP_NAME), fails: (2..=10).collect(), renames: 0, fail: None };
        for (path, v) in [(&d.target, "old"), (&d.staged, "new")] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("version"), v).unwrap();
        }
        let out = run_swap(&mut d, true);
        assert!(
            matches!(&out, SwapOutcome::Stranded { step: Step::Exchange, old: Place::Aside, new: Place::Staged, relaunched: Err(_), .. }),
            "{out:?}"
        );
        assert!(out.keeps_work());
        assert_eq!(Disk::holds(&work.join(ASIDE_NAME)).as_deref(), Some("old"));
    }
}
