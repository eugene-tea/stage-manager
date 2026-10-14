//! Installing an update, pure: what a downloaded app must prove before it may replace the
//! running one, where it may go, and the swap as a transaction that puts the old app back on
//! any failure.
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
            Rejection::NotNewer(v) => write!(f, "its version {v} is not newer than this one"),
        }
    }
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
        Version::parse(version).ok_or("bad version")?;
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
    /// both moved or neither did.
    fn exchange(&mut self) -> Result<(), String>;
    /// Opens the app at the installed path.
    fn launch(&mut self) -> Result<(), String>;
    /// Waits for the app just opened to answer with the expected version.
    fn confirm(&mut self) -> Result<(), String>;
    /// Stops an app opened from the installed path that did not confirm.
    fn stop_launched(&mut self);
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
    /// A step failed and the old app could not be put back: the new one stays and is opened.
    RollbackFailed { step: Step, why: String, undo: String },
}

/// Runs the swap: verify, exchange, launch, confirm, then remove the old app. A failure at
/// any step before the old app is removed puts it back (undoing the exchange) and opens it.
pub fn run_swap(ops: &mut dyn SwapOps) -> SwapOutcome {
    if let Err(why) = ops.verify_staged() {
        return SwapOutcome::RolledBack { step: Step::Verify, why, relaunched: ops.launch() };
    }
    if let Err(why) = ops.exchange() {
        return SwapOutcome::RolledBack { step: Step::Exchange, why, relaunched: ops.launch() };
    }
    let failed = match ops.launch() {
        Err(why) => Some((Step::Launch, why)),
        Ok(()) => ops.confirm().err().map(|why| {
            ops.stop_launched();
            (Step::Confirm, why)
        }),
    };
    if let Some((step, why)) = failed {
        return match ops.exchange() {
            Ok(()) => SwapOutcome::RolledBack { step, why, relaunched: ops.launch() },
            Err(undo) => {
                // The new app stays in place: open it rather than leave no app running.
                let _ = ops.launch();
                SwapOutcome::RollbackFailed { step, why, undo }
            }
        };
    }
    match ops.remove_old() {
        Ok(()) => SwapOutcome::Installed,
        Err(e) => SwapOutcome::InstalledOldLeft(e),
    }
}

/// An exchange of `a` and `b` by three renames through `aside`, for volumes without an atomic
/// swap: `a → aside`, `b → a`, `aside → b`. A failure undoes the renames already made, so
/// either both moved or neither did (unless an undo fails too, which the error says).
pub fn exchange_by_renames(
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    a: &Path,
    b: &Path,
    aside: &Path,
) -> Result<(), String> {
    rename(a, aside).map_err(|e| format!("cannot move {} aside: {e}", a.display()))?;
    if let Err(e) = rename(b, a) {
        let undo = rename(aside, a);
        return Err(match undo {
            Ok(()) => format!("cannot move {} in: {e}", b.display()),
            Err(u) => format!("cannot move {} in: {e}; and cannot put {} back: {u}", b.display(), a.display()),
        });
    }
    if let Err(e) = rename(aside, b) {
        let undo = rename(a, b).and_then(|_| rename(aside, a));
        return Err(match undo {
            Ok(()) => format!("cannot finish the exchange: {e}"),
            Err(u) => format!("cannot finish the exchange: {e}; and cannot undo it: {u}"),
        });
    }
    Ok(())
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

        for older in ["1.1.0", "1.0.9", "1.1.0-rc.1"] {
            let mut c = good();
            c.version = Some(older.into());
            assert!(matches!(judge(&t, &c, &now), Err(Rejection::NotNewer(_))), "{older}");
        }
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
            }
        }
    }

    impl SwapOps for Fake {
        fn verify_staged(&mut self) -> Result<(), String> {
            if self.fail == Some(Step::Verify) { Err("bad".into()) } else { Ok(()) }
        }
        fn exchange(&mut self) -> Result<(), String> {
            self.exchanges += 1;
            let undo_fails = self.fail_exchange_from.is_some_and(|n| self.exchanges >= n);
            if (self.fail == Some(Step::Exchange) && self.exchanges == 1) || undo_fails {
                return Err("no swap".into());
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
        fn stop_launched(&mut self) {
            self.running = None;
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

    /// A fake folder for the three-rename exchange, failing at the nth rename.
    fn renames(fail_at: Option<usize>) -> (HashMap<PathBuf, &'static str>, Result<(), String>) {
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
}
