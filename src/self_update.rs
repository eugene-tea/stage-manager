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
    /// Seconds since the last input event.
    pub idle_secs: f64,
}

/// Whether a ready install may quit the app now: never mid-drag or mid-transition; an
/// automatic one (not asked for by a click) only once the user has been away
/// [`AUTO_IDLE_SECS`].
pub fn may_install_now(asked: bool, m: Moment) -> bool {
    !m.buttons_down && !m.strip_busy && (asked || m.idle_secs >= AUTO_IDLE_SECS)
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
}

impl HelperArgs {
    /// The command-line words after `--update-helper`.
    pub fn to_args(&self) -> Vec<String> {
        vec![
            self.pid.to_string(),
            self.target.to_string_lossy().into_owned(),
            self.staged.to_string_lossy().into_owned(),
            self.version.clone(),
            self.requirement.clone(),
        ]
    }

    /// Parses the words after `--update-helper`; the staged copy must sit in the working
    /// folder beside the target, both named [`APP_NAME`].
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let [pid, target, staged, version, requirement] = args else {
            return Err(format!("expected 5 arguments, got {}", args.len()));
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
        Ok(Self { pid, target, staged, version: version.clone(), requirement: requirement.clone() })
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
    /// The new app is opened.
    Launch,
    /// The new app answers, with the new version.
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
    /// A step failed; the old app is back in place and was opened again (`relaunched`).
    RolledBack { step: Step, why: String, relaunched: Result<(), String> },
    /// A step failed and the old app could not be put back: the new one stays and is opened;
    /// the old one is left at the staged path (the working folder is kept).
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
pub fn run_swap(ops: &mut dyn SwapOps) -> SwapOutcome {
    if let Err(why) = ops.verify_staged() {
        return SwapOutcome::RolledBack { step: Step::Verify, why, relaunched: ops.launch() };
    }
    match ops.exchange() {
        Ok(()) => {}
        Err(ExchangeError::Unchanged(why)) => {
            return SwapOutcome::RolledBack { step: Step::Exchange, why, relaunched: ops.launch() };
        }
        Err(ExchangeError::Stranded { why, was_target, was_staged }) => {
            return stranded(ops, Step::Exchange, why, was_target, was_staged);
        }
    }
    let failed = match ops.launch() {
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
    };
    if let Some((step, why)) = failed {
        return match ops.exchange() {
            Ok(()) => SwapOutcome::RolledBack { step, why, relaunched: ops.launch() },
            Err(ExchangeError::Unchanged(undo)) => {
                // The new app stays in place: open it rather than leave no app running.
                let _ = ops.launch();
                SwapOutcome::RollbackFailed { step, why, undo }
            }
            // The undo's exchange runs with the new app installed: its `was_target` is the new.
            Err(ExchangeError::Stranded { why: undo, was_target, was_staged }) => {
                stranded(ops, step, format!("{why}; and putting the old version back: {undo}"), was_staged, was_target)
            }
        };
    }
    match ops.remove_old() {
        Ok(()) => SwapOutcome::Installed,
        Err(e) => SwapOutcome::InstalledOldLeft(e),
    }
}

/// The outcome of an exchange that stopped part way with the old app at `old` and the new at
/// `new`: rolled back when the old app is at the installed path anyway, else stranded. Either
/// way, whatever is at the installed path is opened.
fn stranded(ops: &mut dyn SwapOps, step: Step, why: String, old: Place, new: Place) -> SwapOutcome {
    let relaunched = ops.launch();
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
        let calm = Moment { buttons_down: false, strip_busy: false, idle_secs: 0.5 };
        assert!(may_install_now(true, calm), "a click on Install and Relaunch goes at once");
        assert!(!may_install_now(false, calm), "an automatic install waits for the user to be away");
        let away = Moment { idle_secs: AUTO_IDLE_SECS, ..calm };
        assert!(may_install_now(false, away));
        for asked in [true, false] {
            assert!(!may_install_now(asked, Moment { buttons_down: true, ..away }), "never mid-drag");
            assert!(!may_install_now(asked, Moment { strip_busy: true, ..away }), "never mid-transition");
        }
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
        };
        assert_eq!(HelperArgs::parse(&a.to_args()), Ok(a.clone()));
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
        assert_eq!(run_swap(&mut f), SwapOutcome::Installed);
        assert_eq!(f.target, Some("new"));
        assert_eq!(f.staged, None, "the old one is removed only at the end");
        assert_eq!(f.launched, vec!["new"]);
        assert_eq!(f.running, Some("new"));
    }

    #[test]
    fn a_failure_at_each_step_puts_the_old_app_back_and_opens_it() {
        for step in [Step::Verify, Step::Exchange, Step::Launch, Step::Confirm] {
            let mut f = Fake::new(Some(step));
            let out = run_swap(&mut f);
            assert!(matches!(&out, SwapOutcome::RolledBack { step: s, relaunched: Ok(()), .. } if *s == step), "{step:?}: {out:?}");
            assert_eq!(f.target, Some("old"), "{step:?}: the old app is back in place");
            assert_eq!(f.staged, Some("new"), "{step:?}: the new copy is set aside, not lost");
            assert_eq!(f.running, Some("old"), "{step:?}: the old app runs again");
            assert_eq!(f.launched.last(), Some(&"old"));
        }
        let mut f = Fake::new(Some(Step::Confirm));
        run_swap(&mut f);
        assert_eq!(f.launched, vec!["new", "old"], "the unconfirmed new app was stopped before the old one opened");
    }

    #[test]
    fn a_failed_removal_keeps_the_new_app() {
        let mut f = Fake::new(Some(Step::RemoveOld));
        assert!(matches!(run_swap(&mut f), SwapOutcome::InstalledOldLeft(_)));
        assert_eq!((f.target, f.staged, f.running), (Some("new"), Some("old"), Some("new")));
    }

    #[test]
    fn a_failed_undo_still_leaves_an_app_running() {
        let mut f = Fake::new(Some(Step::Confirm));
        f.fail_exchange_from = Some(2);
        f.confirm_new_only = true;
        let out = run_swap(&mut f);
        assert!(matches!(out, SwapOutcome::RollbackFailed { step: Step::Confirm, .. }), "{out:?}");
        assert_eq!(f.running, Some("new"), "the new app is opened rather than none");
    }

    /// Review 8C #2: a new app that will not stop is not swapped out from under itself.
    #[test]
    fn an_unstoppable_new_app_is_left_in_place_and_said_so() {
        let mut f = Fake::new(Some(Step::Confirm));
        f.unstoppable = true;
        let out = run_swap(&mut f);
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
        for fail in [None, Some(Step::Launch), Some(Step::Confirm)] {
            for mask in 0u32..(1 << 10) {
                let fails: Vec<usize> = (1..=10).filter(|i| mask & (1 << (i - 1)) != 0).collect();
                let tmp = tempfile::tempdir().unwrap();
                let work = tmp.path().join(WORK_DIR);
                let mut d = Disk { target: tmp.path().join(APP_NAME), staged: work.join(APP_NAME), fails: fails.clone(), renames: 0, fail };
                for (path, v) in [(&d.target, "old"), (&d.staged, "new")] {
                    std::fs::create_dir_all(path).unwrap();
                    std::fs::write(path.join("version"), v).unwrap();
                }
                let out = run_swap(&mut d);
                // The helper's cleanup.
                if matches!(out, SwapOutcome::RolledBack { .. }) {
                    let _ = std::fs::remove_dir_all(&work);
                }
                assert_eq!(out.keeps_work(), work.exists(), "{fail:?} {fails:?}: {out:?}");
                let (target, staged) = (Disk::holds(&d.target), Disk::holds(&d.staged));
                let ctx = format!("{fail:?} {fails:?}: {out:?}");
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
                        assert!(*new == Place::Target || relaunched.is_err(), "{ctx}: nothing to open");
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
        let out = run_swap(&mut d);
        assert!(
            matches!(&out, SwapOutcome::Stranded { step: Step::Exchange, old: Place::Aside, new: Place::Staged, relaunched: Err(_), .. }),
            "{out:?}"
        );
        assert!(out.keeps_work());
        assert_eq!(Disk::holds(&work.join(ASIDE_NAME)).as_deref(), Some("old"));
    }
}
