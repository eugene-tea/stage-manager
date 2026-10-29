//! Installing an update (8C): download the release's disk image, verify the app inside
//! before anything is replaced, and hand the swap to a helper process at quit.
//!
//! 1. **Download** ([`download`]): an ephemeral `NSURLSession` (no cookies, no cache, no
//!    credentials) fetches the release's `Stage-Manager.dmg` from `github.com`; a delegate
//!    refuses every redirect off [`updates::DOWNLOAD_HOSTS`], and the final answer's host is
//!    checked again. The image lands in a private temporary folder (`mkdtemp`, mode 0700).
//! 2. **Verify** ([`prepare`]): the image's own signature must be our Developer ID; it is
//!    mounted read-only and hidden (`hdiutil attach -nobrowse -readonly -noautoopen`); the
//!    app inside must pass [`self_update::judge`] (signature, our designated requirement and
//!    team, notarized, same bundle ID, newer). It is copied beside the installed app (the
//!    same volume, so the swap is a rename) and the copy is judged again; only then is its
//!    quarantine cleared. Any failure: nothing is replaced, the reason is logged and shown.
//! 3. **Hand off** ([`hand_off`], [`launch_pending_helper`]): the app quits through its
//!    normal quit path (every window back, the leftovers check), and right before it exits
//!    it starts the helper: this same binary, already running and trusted, in its
//!    `--update-helper` mode ([`run_helper`]). The new code never runs before the swap.
//! 4. **Swap** ([`run_helper`]): the helper waits for the old process to exit, checks the
//!    staged copy's signature once more, exchanges it with the installed app atomically
//!    (`renamex_np(RENAME_SWAP)`, or three renames that undo themselves), opens it, waits
//!    for it to answer on the ctl socket with the new version, and only then removes the old
//!    app. Any failure puts the old app back and opens it ([`self_update::run_swap`]), and
//!    leaves a note the app shows at its next launch ([`take_note`]). When the old app cannot
//!    be put back, the working folder is kept (marked, so a launch does not clean it) and the
//!    log and the note say where each copy is (review R1). The old process is told by its pid
//!    and its start time, never a pid reused since (review C3); when nothing could be opened at
//!    the installed path (the app was moved since), the app is opened by its bundle ID (C12).
//!    A disk image kept for a manual install goes an hour later (C11).
//!
//! Test builds only (the `update-test` feature): `STAGE_MANAGER_UPDATE_DMG=<file>` takes a
//! local disk image instead of downloading one, and a running app with a local (not
//! Developer ID) signature trusts its own designated requirement, so the whole flow can run
//! without GitHub. Release builds compile neither.

use std::ffi::CString;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_foundation::{
    NSBundle, NSError, NSHTTPCookieAcceptPolicy, NSHTTPURLResponse, NSMutableURLRequest, NSObject, NSObjectProtocol,
    NSString, NSURL, NSURLRequest, NSURLRequestCachePolicy, NSURLResponse, NSURLSession, NSURLSessionConfiguration,
    NSURLSessionDelegate, NSURLSessionTask, NSURLSessionTaskDelegate,
};

use super::{ctl, logging, paths};
use crate::self_update::{
    self, Assessment, Candidate, ExchangeError, HelperArgs, Place, Refusal, SwapOps, SwapOutcome, Trust, APP_NAME,
};
use crate::updates::{self, Release, Version};

/// How long one request may wait for data.
const REQUEST_TIMEOUT_SECS: f64 = 60.0;
/// How long the whole download may take.
const DOWNLOAD_TIMEOUT_SECS: f64 = 600.0;
/// How long a tool (`codesign`, `spctl`, `hdiutil`, `ditto`) may run.
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the helper waits for the old app to exit.
const EXIT_WAIT: Duration = Duration::from_secs(90);
/// How long the helper waits for the new app to answer.
const CONFIRM_WAIT: Duration = Duration::from_secs(45);
/// How long the helper waits for an unconfirmed new app to quit.
const STOP_WAIT: Duration = Duration::from_secs(45);
/// How long the helper waits for it to go after a kill.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// How often the download's size is looked at while it arrives.
const DOWNLOAD_POLL: Duration = Duration::from_millis(100);
/// The disk image's identifier (`scripts/release.sh` signs it with `-i`).
const DMG_IDENTIFIER_SUFFIX: &str = ".dmg";
/// The note the helper leaves when an update did not go in, read at the next launch.
const NOTE_FILE: &str = "update-result.txt";

/// Test builds only: a local disk image instead of the download.
#[cfg(feature = "update-test")]
pub const TEST_DMG_ENV: &str = "STAGE_MANAGER_UPDATE_DMG";

/// Whether a running app with a local signature may update itself from a local image.
const ALLOW_LOCAL_TRUST: bool = cfg!(feature = "update-test");

// ---------------------------------------------------------------------------------------
// Tools

/// Runs `program` with `args` (no shell), at most `timeout`; returns whether it exited 0 and
/// its stdout and stderr together.
fn tool(program: &str, args: &[&std::ffi::OsStr], timeout: Duration) -> Result<(bool, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut text);
        }
        text
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut text);
        }
        text
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} did not finish in {} s", timeout.as_secs()));
            }
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    let mut text = reader.join().unwrap_or_default();
    text.push_str(&err_reader.join().unwrap_or_default());
    Ok((status.success(), text))
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

/// `codesign -dv --verbose=2`.
fn code_info(path: &Path) -> Result<self_update::CodeInfo, String> {
    let (_, text) = tool("/usr/bin/codesign", &[os("-dv"), os("--verbose=2"), path.as_os_str()], TOOL_TIMEOUT)?;
    Ok(self_update::parse_codesign_display(&text))
}

/// `codesign -d -r-`: the designated requirement.
fn designated(path: &Path) -> Option<String> {
    tool("/usr/bin/codesign", &[os("-d"), os("-r-"), path.as_os_str()], TOOL_TIMEOUT)
        .ok()
        .and_then(|(_, text)| self_update::parse_designated(&text))
}

/// `codesign --verify --deep --strict -R=<requirement>`.
fn verify_signature(path: &Path, requirement: &str) -> Result<(), String> {
    let req = format!("={requirement}");
    let (ok, text) = tool(
        "/usr/bin/codesign",
        &[os("--verify"), os("--deep"), os("--strict"), os("-R"), os(&req), path.as_os_str()],
        TOOL_TIMEOUT,
    )?;
    if ok { Ok(()) } else { Err(text.trim().lines().last().unwrap_or("codesign refused it").to_string()) }
}

/// `spctl --assess --type execute -vv`: Gatekeeper's view of an app. The release staples the
/// notarization ticket to the app itself (`scripts/release.sh`), so this holds offline too;
/// an unstapled app would need Apple's servers to be reachable.
fn assess(path: &Path) -> Assessment {
    match tool("/usr/sbin/spctl", &[os("--assess"), os("--type"), os("execute"), os("-vv"), path.as_os_str()], TOOL_TIMEOUT) {
        Ok((ok, text)) => self_update::parse_spctl(&text, ok),
        Err(e) => {
            tracing::warn!("update: spctl: {e}");
            Assessment::default()
        }
    }
}

/// A key of an app's Info.plist.
fn plist_value(app: &Path, key: &str) -> Option<String> {
    let plist = app.join("Contents/Info.plist");
    tool("/usr/bin/plutil", &[os("-extract"), os(key), os("raw"), os("-o"), os("-"), plist.as_os_str()], TOOL_TIMEOUT)
        .ok()
        .filter(|(ok, _)| *ok)
        .map(|(_, t)| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Everything [`self_update::judge`] needs about the app at `path`.
fn candidate(path: &Path, trust: &Trust) -> Candidate {
    Candidate {
        info: code_info(path).unwrap_or_default(),
        signature: verify_signature(path, &trust.requirement),
        assessment: if trust.strict { assess(path) } else { Assessment::default() },
        bundle_id: plist_value(path, "CFBundleIdentifier"),
        version: plist_value(path, "CFBundleShortVersionString"),
    }
}

// ---------------------------------------------------------------------------------------
// The running app

/// The running app's bundle.
pub fn own_bundle() -> PathBuf {
    PathBuf::from(NSBundle::mainBundle().bundlePath().to_string())
}

/// The running app's bundle ID.
fn own_bundle_id() -> String {
    NSBundle::mainBundle().bundleIdentifier().map(|s| s.to_string()).unwrap_or_default()
}

/// What the running app trusts, read once from its own signature.
fn own_trust() -> Result<Trust, Refusal> {
    static TRUST: OnceLock<Result<Trust, Refusal>> = OnceLock::new();
    TRUST
        .get_or_init(|| {
            let bundle = own_bundle();
            let info = code_info(&bundle).map_err(Refusal::NoOwnSignature)?;
            let trust = Trust::from_own(&info, designated(&bundle).as_deref(), &own_bundle_id(), ALLOW_LOCAL_TRUST);
            match &trust {
                Ok(t) => tracing::info!(team = ?t.team_id, strict = t.strict, "update: this app can install updates"),
                Err(r) => tracing::info!(refusal = ?r, "update: this app cannot install updates itself"),
            }
            trust
        })
        .clone()
}

/// Whether this user may write `dir`.
fn writable(dir: &Path) -> bool {
    let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else { return false };
    // SAFETY: a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Where an update would go, or why this copy cannot install one itself (a local build, a
/// translocated or disk-image copy: Download instead; a folder it may not write: the
/// verified disk image opens for a manual install).
pub fn can_install() -> Result<PathBuf, Refusal> {
    let bundle = own_bundle();
    let parent_writable = bundle.parent().is_some_and(writable);
    let target = self_update::install_target(&bundle, parent_writable);
    if let Err(r) = &target
        && !r.opens_disk_image()
    {
        return target;
    }
    // A copy whose signature is not ours to trust never downloads anything.
    own_trust()?;
    target
}

/// Reads the running app's own signature off the main thread (the first offer then needs no
/// `codesign` run).
pub fn warm() {
    let _ = std::thread::Builder::new().name("update-trust".into()).spawn(|| {
        let _ = can_install();
    });
}

// ---------------------------------------------------------------------------------------
// Download

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no ivars and no Drop.
    #[unsafe(super = NSObject)]
    #[name = "SMUpdateDownloadDelegate"]
    struct RedirectGuard;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for RedirectGuard {}

    // SAFETY: the delegate methods below match the protocol's signatures.
    unsafe impl NSURLSessionDelegate for RedirectGuard {}

    // SAFETY: as above.
    unsafe impl NSURLSessionTaskDelegate for RedirectGuard {
        /// Follows a redirect only to [`updates::DOWNLOAD_HOSTS`]; anything else ends the
        /// task with the redirect answer (which then fails as not 200).
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn will_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            request: &NSURLRequest,
            completion: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            let url = request.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            if updates::download_host_allowed(&url) {
                tracing::debug!(host = ?updates::https_host(&url), "update: redirect followed");
                completion.call((request as *const NSURLRequest as *mut NSURLRequest,));
            } else {
                tracing::warn!(host = ?updates::https_host(&url), "update: a redirect off GitHub refused");
                completion.call((std::ptr::null_mut(),));
            }
        }
    }
);

// SAFETY: the class has no state; NSURLSession calls it from its own queue.
unsafe impl Send for RedirectGuard {}
// SAFETY: as above.
unsafe impl Sync for RedirectGuard {}

impl RedirectGuard {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's init on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Downloads `url` (the release's disk image) to `dest`, blocking this (background) thread.
fn download(url: &str, dest: &Path, size_hint: Option<u64>) -> Result<(), String> {
    if !updates::asset_url_allowed(url) || !updates::download_host_allowed(url) {
        return Err(format!("refused to download from {url}"));
    }
    if size_hint.is_some_and(|s| s > updates::MAX_DMG_BYTES) {
        return Err("the disk image is too large".into());
    }
    let ns_url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("bad URL")?;
    let request = NSMutableURLRequest::requestWithURL_cachePolicy_timeoutInterval(
        &ns_url,
        NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
        REQUEST_TIMEOUT_SECS,
    );
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str("application/octet-stream")), &NSString::from_str("Accept"));
    request.setValue_forHTTPHeaderField(
        Some(&NSString::from_str(&format!("Stage-Manager/{}", env!("CARGO_PKG_VERSION")))),
        &NSString::from_str("User-Agent"),
    );
    request.setHTTPShouldHandleCookies(false);
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    config.setHTTPCookieStorage(None);
    config.setHTTPShouldSetCookies(false);
    config.setHTTPCookieAcceptPolicy(NSHTTPCookieAcceptPolicy::Never);
    config.setURLCache(None);
    config.setURLCredentialStorage(None);
    config.setTimeoutIntervalForRequest(REQUEST_TIMEOUT_SECS);
    config.setTimeoutIntervalForResource(DOWNLOAD_TIMEOUT_SECS);
    let guard = RedirectGuard::new();
    // SAFETY: a valid configuration and delegate; the session keeps the delegate until it is
    // invalidated (below, once the task ends); a nil queue makes it a serial one of its own.
    let session = unsafe {
        NSURLSession::sessionWithConfiguration_delegate_delegateQueue(&config, Some(ProtocolObject::from_ref(&*guard)), None)
    };
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let tx = Mutex::new(Some(tx));
    let dest = dest.to_path_buf();
    let handler = RcBlock::new(move |location: *mut NSURL, response: *mut NSURLResponse, error: *mut NSError| {
        // SAFETY: NSURLSession passes valid (possibly nil) objects for the block's duration.
        let (location, response, error) = unsafe { (location.as_ref(), response.as_ref(), error.as_ref()) };
        let result = (|| {
            if let Some(e) = error {
                return Err(e.localizedDescription().to_string());
            }
            let http = response.and_then(|r| r.downcast_ref::<NSHTTPURLResponse>()).ok_or("no HTTP answer")?;
            let final_url = http.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            if !updates::download_host_allowed(&final_url) {
                return Err(format!("the download ended on another host ({:?})", updates::https_host(&final_url)));
            }
            let status = http.statusCode();
            if status != 200 {
                return Err(format!("GitHub answered HTTP {status}"));
            }
            let from = location.and_then(|l| l.path()).map(|p| PathBuf::from(p.to_string())).ok_or("no file")?;
            // The file is deleted when this block returns: move it now.
            if std::fs::rename(&from, &dest).is_err() {
                std::fs::copy(&from, &dest).map_err(|e| format!("cannot keep the download: {e}"))?;
            }
            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            if size == 0 || size > updates::MAX_DMG_BYTES {
                return Err(format!("the disk image has an unexpected size ({size} bytes)"));
            }
            Ok(())
        })();
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(result);
        }
    });
    // SAFETY: a valid request and a completion block that lives as long as the task needs it.
    let task = unsafe { session.downloadTaskWithRequest_completionHandler(&request, &handler) };
    task.resume();
    session.finishTasksAndInvalidate();
    // The size cap holds while the image arrives (review 8C #8): what came so far, or what the
    // server announced, past MAX_DMG_BYTES cancels the task at once.
    let wait = Duration::from_secs_f64(DOWNLOAD_TIMEOUT_SECS + 15.0);
    let start = Instant::now();
    let stopped = loop {
        match rx.recv_timeout(DOWNLOAD_POLL) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Disconnected) => break "the download ended without an answer",
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if updates::download_too_large(task.countOfBytesReceived(), task.countOfBytesExpectedToReceive()) {
            break "the disk image is too large";
        }
        if start.elapsed() > wait {
            break "the download did not finish";
        }
    };
    task.cancel();
    // The cancelled task's handler ends with an error; wait for it so nothing lands in `dest`
    // after this returns.
    let _ = rx.recv_timeout(Duration::from_secs(5));
    Err(stopped.into())
}

// ---------------------------------------------------------------------------------------
// Prepare

/// A verified update, staged beside the installed app.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// Its version.
    pub version: Version,
    /// What the helper is told.
    pub helper: HelperArgs,
}

/// Why an update did not get ready.
#[derive(Debug, Clone)]
pub enum PrepareError {
    /// It was verified, but this copy cannot put it in place: the disk image (kept) is for a
    /// manual install.
    Manual(Refusal, PathBuf),
    /// It failed (download, mount, verification); nothing was replaced.
    Failed(String),
}

/// The prefix of an update's private temporary folder ([`private_dir`]).
const DOWNLOAD_DIR_PREFIX: &str = "stage-manager-update.";

/// How long a disk image kept for a manual install stays: it is opened in the Finder at
/// once, and a mounted image needs no file.
const MANUAL_KEEP: Duration = Duration::from_secs(60 * 60);

/// A private temporary folder (mode 0700).
fn private_dir() -> Result<PathBuf, String> {
    let template = paths::user_temp_dir().join(format!("{DOWNLOAD_DIR_PREFIX}XXXXXX"));
    let mut bytes = template.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: a writable NUL-terminated template ending in XXXXXX.
    let p = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if p.is_null() {
        return Err(format!("cannot make a private folder: {}", std::io::Error::last_os_error()));
    }
    bytes.pop();
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

/// A mounted disk image, detached when dropped.
struct Mounted(PathBuf);

impl Mounted {
    fn attach(dmg: &Path, at: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(at).map_err(|e| format!("cannot make the mount point: {e}"))?;
        let (ok, text) = tool(
            "/usr/bin/hdiutil",
            &[os("attach"), os("-nobrowse"), os("-readonly"), os("-noautoopen"), os("-mountpoint"), at.as_os_str(), dmg.as_os_str()],
            TOOL_TIMEOUT,
        )?;
        if !ok {
            return Err(format!("cannot open the disk image: {}", text.trim()));
        }
        Ok(Self(at.to_path_buf()))
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let detached = tool("/usr/bin/hdiutil", &[os("detach"), self.0.as_os_str()], TOOL_TIMEOUT).is_ok_and(|(ok, _)| ok)
            || tool("/usr/bin/hdiutil", &[os("detach"), os("-force"), self.0.as_os_str()], TOOL_TIMEOUT).is_ok_and(|(ok, _)| ok);
        if !detached {
            tracing::warn!("update: cannot detach the disk image at {}", self.0.display());
        }
    }
}

/// Removes `com.apple.quarantine` from `path` and everything inside (never following links).
fn clear_quarantine(path: &Path) {
    let name = c"com.apple.quarantine";
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return };
    // SAFETY: valid NUL-terminated strings; a missing attribute is ENOATTR, ignored.
    unsafe { libc::removexattr(c.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    if is_dir && let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            clear_quarantine(&e.path());
        }
    }
}

/// Removes `path` (a folder, or a stray file or link) if it exists.
fn remove_any(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Puts the disk image for `release` in `dir`: the download, or (test builds) a local file.
fn fetch(release: &Release, dir: &Path) -> Result<PathBuf, String> {
    let dmg = dir.join(updates::DMG_NAME);
    #[cfg(feature = "update-test")]
    if let Some(local) = std::env::var_os(TEST_DMG_ENV).filter(|v| !v.is_empty()) {
        tracing::warn!("update: test build: the disk image comes from {}", PathBuf::from(&local).display());
        std::fs::copy(&local, &dmg).map_err(|e| format!("cannot copy the test disk image: {e}"))?;
        return Ok(dmg);
    }
    let url = release.dmg.as_deref().ok_or("this release has no Stage-Manager.dmg")?;
    download(url, &dmg, release.dmg_size)?;
    Ok(dmg)
}

/// Removes the update folders in `parent` older than `keep` (by their last change): the disk
/// images kept for manual installs, which nothing else removes (review C11). A younger one
/// may be another copy's download in progress, and stays.
fn clean_old_downloads(parent: &Path, keep: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    let mut removed = 0;
    for e in entries.flatten() {
        let ours = e.file_name().to_str().is_some_and(|n| n.starts_with(DOWNLOAD_DIR_PREFIX));
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        let old = meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= keep);
        if ours && meta.is_dir() && old && remove_any(&e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Downloads and verifies `release` (blocking; a background thread). See the module docs.
fn prepare_blocking(release: &Release) -> Result<Prepared, PrepareError> {
    let old = clean_old_downloads(&paths::user_temp_dir(), MANUAL_KEEP);
    if old > 0 {
        tracing::info!(folders = old, "update: removed the disk images kept for earlier manual installs");
    }
    let trust = own_trust().map_err(|r| PrepareError::Failed(format!("{r:?}")))?;
    let target = can_install();
    let dir = private_dir().map_err(PrepareError::Failed)?;
    let result = prepare_in(release, &trust, target, &dir);
    // The private folder goes, unless its disk image is kept for a manual install.
    if !matches!(result, Err(PrepareError::Manual(..))) {
        let _ = remove_any(&dir);
    }
    result
}

fn prepare_in(release: &Release, trust: &Trust, target: Result<PathBuf, Refusal>, dir: &Path) -> Result<Prepared, PrepareError> {
    let failed = PrepareError::Failed;
    let dmg = fetch(release, dir).map_err(failed)?;
    tracing::info!(version = %release.version, "update: downloaded; verifying");
    if trust.strict {
        let team = trust.team_id.as_deref().unwrap_or_default();
        let dmg_id = format!("{}{DMG_IDENTIFIER_SUFFIX}", trust.bundle_id);
        verify_signature(&dmg, &self_update::developer_id_requirement(&dmg_id, team))
            .map_err(|e| failed(format!("the disk image is not signed by us: {e}")))?;
    }
    let current = updates::current();
    let mounted = Mounted::attach(&dmg, &dir.join("mnt")).map_err(failed)?;
    let app = mounted.0.join(APP_NAME);
    if !std::fs::symlink_metadata(&app).is_ok_and(|m| m.is_dir()) {
        return Err(failed(format!("the disk image has no {APP_NAME}")));
    }
    let version = self_update::judge(trust, &candidate(&app, trust), &current).map_err(|r| failed(r.to_string()))?;
    tracing::info!(%version, "update: the app on the disk image is verified");
    let target = match target {
        Ok(t) => t,
        Err(r) => {
            drop(mounted);
            return Err(PrepareError::Manual(r, dmg));
        }
    };
    let (work, staged) = self_update::staging_paths(&target).ok_or_else(|| failed("no folder for the copy".into()))?;
    remove_any(&work).map_err(|e| failed(format!("cannot clear {}: {e}", work.display())))?;
    std::fs::create_dir(&work).map_err(|e| failed(format!("cannot make {}: {e}", work.display())))?;
    let copied = tool("/usr/bin/ditto", &[app.as_os_str(), staged.as_os_str()], TOOL_TIMEOUT);
    drop(mounted);
    match copied {
        Ok((true, _)) => {}
        Ok((false, text)) => {
            let _ = remove_any(&work);
            return Err(failed(format!("cannot copy the new version: {}", text.trim())));
        }
        Err(e) => {
            let _ = remove_any(&work);
            return Err(failed(e));
        }
    }
    // The copy that will be installed, judged again: what was checked is what goes in.
    if let Err(r) = self_update::judge(trust, &candidate(&staged, trust), &current) {
        let _ = remove_any(&work);
        return Err(failed(format!("the copy did not verify: {r}")));
    }
    clear_quarantine(&staged);
    let helper = HelperArgs { pid: std::process::id(), target, staged, version: version.to_string(), requirement: trust.requirement.clone() };
    Ok(Prepared { version, helper })
}

/// Downloads and verifies `release` off the main thread; `done` runs on the main thread.
pub fn prepare(release: Release, done: impl FnOnce(Result<Prepared, PrepareError>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        let r = prepare_blocking(&release);
        match &r {
            Ok(p) => tracing::info!(version = %p.version, staged = %p.helper.staged.display(), "update: ready to install"),
            Err(e) => tracing::warn!(error = ?e, "update: not installed"),
        }
        dispatch2::DispatchQueue::main().exec_async(move || done(r));
    });
    if let Err(e) = spawned {
        tracing::warn!("update: cannot start: {e}");
    }
}

/// Opens a kept disk image in the Finder (a manual install).
pub fn open_disk_image(dmg: &Path) {
    if let Err(e) = Command::new("/usr/bin/open").arg(dmg).status() {
        tracing::warn!("update: cannot open {}: {e}", dmg.display());
    }
}

/// Removes a ready update's staged copy (an install that will not happen now): its working
/// folder beside the installed app goes, off the main thread.
pub fn discard(p: &Prepared) {
    let Some(work) = p.helper.staged.parent().filter(|w| w.file_name().is_some_and(|n| n == self_update::WORK_DIR)) else { return };
    let work = work.to_path_buf();
    let _ = std::thread::Builder::new().name("update-discard".into()).spawn(move || match remove_any(&work) {
        Ok(()) => tracing::info!("update: the staged copy is removed"),
        Err(e) => tracing::warn!("update: cannot remove {}: {e}", work.display()),
    });
}

/// At launch, off the main thread: removes a working folder an earlier update left beside
/// this app (the app quit while an update waited, or a helper was killed), unless an update
/// helper runs now (the folder is its own until it ends); and the disk images kept for
/// manual installs more than an hour ago.
pub fn clean_stale_work() {
    let _ = std::thread::Builder::new().name("update-clean".into()).spawn(|| {
        let old = clean_old_downloads(&paths::user_temp_dir(), MANUAL_KEEP);
        if old > 0 {
            tracing::info!(folders = old, "update: removed the disk images kept for earlier manual installs");
        }
        let Ok(target) = self_update::install_target(&own_bundle(), true) else { return };
        let Some((work, _)) = self_update::staging_paths(&target) else { return };
        if std::fs::symlink_metadata(&work).is_err() {
            return;
        }
        if processes(|_, argv| self_update::is_update_helper(argv)).next().is_some() {
            tracing::info!("update: an installer helper runs; its working folder stays");
            return;
        }
        if work.join(self_update::KEEP_MARK).exists() {
            tracing::warn!("update: {} holds a copy an update could not put back; it stays until the next update", work.display());
            return;
        }
        match remove_any(&work) {
            Ok(()) => tracing::info!("update: removed a working folder an earlier update left behind"),
            Err(e) => tracing::warn!("update: cannot remove {}: {e}", work.display()),
        }
    });
}

// ---------------------------------------------------------------------------------------
// Hand-off

fn pending() -> &'static Mutex<Option<HelperArgs>> {
    static P: OnceLock<Mutex<Option<HelperArgs>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(None))
}

/// Arms the helper: the next exit (the quit the caller starts now) launches it.
pub fn hand_off(p: &Prepared) {
    if let Ok(mut slot) = pending().lock() {
        *slot = Some(p.helper.clone());
    }
}

/// Starts the armed helper, if any (the app's exit path, after every window came back).
/// Returns whether one started.
pub fn launch_pending_helper() -> bool {
    let Some(args) = pending().lock().ok().and_then(|mut s| s.take()) else { return false };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("update: cannot find our own executable: {e}");
            return false;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--update-helper").args(args.to_args()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches nothing shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => {
            tracing::info!(pid = child.id(), version = %args.version, "update: the installer helper started");
            true
        }
        Err(e) => {
            tracing::error!("update: cannot start the installer helper: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------------------
// The helper

/// The note left for the next launch.
fn note_path() -> PathBuf {
    paths::app_state_file().with_file_name(NOTE_FILE)
}

/// The note the helper left (an update that did not go in), removed once read.
pub fn take_note() -> Option<String> {
    let path = note_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Whether the app `pid`, which started at `born` (when the helper could read it), still
/// runs: the pid alive and its start time unchanged, so a pid the system gave to another
/// process after the app exited does not hold the update back (review C3).
fn app_still_runs(pid: u32, born: Option<(u64, u64)>) -> bool {
    alive(pid) && same_start(born, start_time(pid as i32))
}

/// A process seen now (`now`, its start time when readable) is the one that started at
/// `born`; with no start time read at first, any process at the pid is.
fn same_start(born: Option<(u64, u64)>, now: Option<(u64, u64)>) -> bool {
    born.is_none_or(|b| now == Some(b))
}

/// A process's executable.
fn exe_of(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes long.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (len > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len as usize])))
}

/// A process's arguments (`sysctl KERN_PROCARGS2`; this user's processes only).
fn argv_of(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query (null buffer) for a valid three-word name.
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: the buffer holds `size` bytes, which is passed.
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    buf.truncate(size);
    self_update::parse_procargs2(&buf)
}

/// When a process started (seconds since 1970).
fn started_at(pid: i32) -> Option<u64> {
    start_time(pid).map(|(secs, _)| secs)
}

/// When a process started: seconds since 1970 and microseconds.
fn start_time(pid: i32) -> Option<(u64, u64)> {
    // SAFETY: a zeroed proc_bsdinfo is valid; proc_pidinfo fills at most its size.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: as above.
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// Other processes running the app's executable (`…/Contents/MacOS/stage-manager`, from any
/// bundle) that `keep(exe, argv)` accepts.
fn processes(keep: impl Fn(&Path, &[String]) -> bool) -> impl Iterator<Item = i32> {
    let mut pids = vec![0i32; 8192];
    // SAFETY: the buffer holds `pids.len()` ints and its size in bytes is passed.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as i32) };
    pids.truncate(usize::try_from(n).unwrap_or(0).min(pids.len()));
    pids.into_iter().filter(move |&pid| {
        pid > 0
            && pid as u32 != std::process::id()
            && exe_of(pid).is_some_and(|exe| {
                exe.ends_with("Contents/MacOS/stage-manager") && argv_of(pid).is_some_and(|argv| keep(&exe, &argv))
            })
    })
}

/// Restores the signals the helper ignored while it renamed bundles.
struct SignalsIgnored(Vec<(libc::c_int, libc::sighandler_t)>);

impl SignalsIgnored {
    /// Ignores SIGTERM, SIGHUP and SIGINT until dropped: a logout or a stray signal cannot stop
    /// the helper halfway through an exchange (review 8C #5). SIGKILL still can (it cannot be
    /// caught); the next launch then removes what is left in the working folder.
    fn now() -> Self {
        Self(
            [libc::SIGTERM, libc::SIGHUP, libc::SIGINT]
                .into_iter()
                // SAFETY: SIG_IGN for a catchable signal; the previous handler is kept to restore.
                .map(|sig| (sig, unsafe { libc::signal(sig, libc::SIG_IGN) }))
                .collect(),
        )
    }
}

impl Drop for SignalsIgnored {
    fn drop(&mut self) {
        for &(sig, previous) in &self.0 {
            if previous != libc::SIG_ERR {
                // SAFETY: puts back the handler `signal` returned for this signal.
                unsafe { libc::signal(sig, previous) };
            }
        }
    }
}

/// The helper's real operations.
struct RealSwap {
    args: HelperArgs,
    /// The new app's process, once it answered.
    new_pid: Option<u32>,
    /// When the helper opened the app (seconds since 1970, a second early): an instance found
    /// without an answer must have started since.
    launched_at: Option<u64>,
}

impl RealSwap {
    fn exe(&self) -> PathBuf {
        self.args.target.join("Contents/MacOS/stage-manager")
    }

    /// The app the helper opened: the one that answered, else the app instances (no
    /// subcommand: never the user's CLI, the guardian or a helper) of the installed
    /// executable started since the launch (review 8C #7).
    fn launched_pids(&self) -> Vec<i32> {
        if let Some(p) = self.new_pid {
            return vec![p as i32];
        }
        let Some(since) = self.launched_at else { return Vec::new() };
        let exe = self.exe();
        processes(|e, argv| e == exe && self_update::is_app_instance(argv))
            .filter(|&pid| pid as u32 != self.args.pid && started_at(pid).is_some_and(|t| t >= since))
            .collect()
    }
}

/// Sends `sig` to each of `pids`, then waits up to `wait` for them to go. Returns whether
/// they all went.
fn signal_and_wait(pids: &[i32], sig: libc::c_int, wait: Duration) -> bool {
    for &pid in pids {
        if alive(pid as u32) {
            // SAFETY: a plain signal to the instance the helper opened.
            unsafe { libc::kill(pid, sig) };
        }
    }
    let start = Instant::now();
    while pids.iter().any(|&p| alive(p as u32)) {
        if start.elapsed() >= wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

impl SwapOps for RealSwap {
    fn verify_staged(&mut self) -> Result<(), String> {
        verify_signature(&self.args.staged, &self.args.requirement)?;
        match plist_value(&self.args.staged, "CFBundleShortVersionString") {
            Some(v) if self_update::same_version(&v, &self.args.version) => Ok(()),
            other => Err(format!("the staged copy's version is {other:?}, not {}", self.args.version)),
        }
    }

    fn exchange(&mut self) -> Result<(), ExchangeError> {
        let _signals = SignalsIgnored::now();
        let (a, b) = (&self.args.target, &self.args.staged);
        let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(|e| ExchangeError::Unchanged(e.to_string()));
        let (ca, cb) = (c(a)?, c(b)?);
        // SAFETY: two valid NUL-terminated paths.
        if unsafe { libc::renamex_np(cb.as_ptr(), ca.as_ptr(), libc::RENAME_SWAP) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        tracing::info!("updater: no atomic swap here ({err}); exchanging by renames");
        let aside = Place::Aside.path(a, b);
        let _ = remove_any(&aside);
        self_update::exchange_by_renames(&mut |from, to| std::fs::rename(from, to), a, b, &aside)
    }

    fn launch(&mut self) -> Result<(), String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.launched_at = Some(now.saturating_sub(1));
        self.new_pid = None;
        let mut open = Command::new("/usr/bin/open");
        open.arg("-n");
        for var in self_update::relaunch_env(std::env::vars()) {
            open.arg("--env").arg(var);
        }
        let status = open.arg(&self.args.target).status().map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err(format!("open exited with {status}")) }
    }

    fn confirm(&mut self) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < CONFIRM_WAIT {
            if let Ok(reply) = ctl::request("ping", Duration::from_secs(2))
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&reply)
            {
                let pid = v["pid"].as_u64().map(|p| p as u32);
                let version_ok = v["version"].as_str().is_some_and(|v| self_update::same_version(v, &self.args.version));
                if version_ok && pid.is_some_and(|p| p != self.args.pid) {
                    self.new_pid = pid;
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(format!("the new version did not answer within {} s", CONFIRM_WAIT.as_secs()))
    }

    fn stop_launched(&mut self) -> bool {
        // Its own quit path (SIGTERM) brings back any window it parked; one that ignores it is
        // killed (review 8C #2), and its guardian (its child, `--guardian`, never signalled
        // here) brings the windows back after it. Survivors: the caller undoes nothing.
        let pids = self.launched_pids();
        tracing::info!(?pids, "updater: stopping the new version that did not answer");
        if signal_and_wait(&pids, libc::SIGTERM, STOP_WAIT) {
            return true;
        }
        tracing::warn!(?pids, "updater: the new version ignored the quit; killing it");
        let gone = signal_and_wait(&pids, libc::SIGKILL, KILL_WAIT);
        if !gone {
            tracing::error!(?pids, "updater: the new version is still running");
        }
        gone
    }

    fn remove_old(&mut self) -> Result<(), String> {
        let work = self.args.staged.parent().ok_or("no working folder")?;
        remove_any(work).map_err(|e| format!("cannot remove {}: {e}", work.display()))
    }
}

/// Whether the swap ended with no app opened: a roll back or a stranded exchange whose
/// relaunch at the installed path failed.
fn relaunch_failed(outcome: &SwapOutcome) -> bool {
    matches!(outcome, SwapOutcome::RolledBack { relaunched: Err(_), .. } | SwapOutcome::Stranded { relaunched: Err(_), .. })
}

/// Opens the app by its bundle ID (`open -b`), with the variables a relaunch passes on.
fn reopen_by_bundle_id() -> Result<(), String> {
    let id = own_bundle_id();
    if id.is_empty() {
        return Err("no bundle ID".into());
    }
    let mut open = Command::new("/usr/bin/open");
    for var in self_update::relaunch_env(std::env::vars()) {
        open.arg("--env").arg(var);
    }
    let status = open.arg("-b").arg(&id).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("open exited with {status}")) }
}

/// `stage-manager --update-helper <pid> <target> <staged> <version> <requirement>`: waits
/// for the old app to exit, then swaps in the verified copy (module docs).
pub fn run_helper(args: &[String]) -> ExitCode {
    logging::init(logging::Process::Tool { name: "updater", echo: false });
    let args = match HelperArgs::parse(args) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("updater: {e}");
            return ExitCode::from(2);
        }
    };
    let born = start_time(args.pid as i32);
    tracing::info!(pid = args.pid, version = %args.version, target = %args.target.display(), "updater: waiting for the app to quit");
    let start = Instant::now();
    while app_still_runs(args.pid, born) {
        if start.elapsed() > EXIT_WAIT {
            tracing::error!("updater: the app did not quit; nothing replaced");
            let _ = remove_any(args.staged.parent().unwrap_or(&args.staged));
            let current = plist_value(&args.target, "CFBundleShortVersionString").unwrap_or_default();
            let _ = std::fs::write(
                note_path(),
                format!("Stage Manager {} could not be installed (the app did not quit in time), so you still have {current}.", args.version),
            );
            return ExitCode::FAILURE;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut ops = RealSwap { args: args.clone(), new_pid: None, launched_at: None };
    let outcome = self_update::run_swap(&mut ops);
    let current = || plist_value(&args.target, "CFBundleShortVersionString").unwrap_or_default();
    match &outcome {
        SwapOutcome::Installed => tracing::info!(version = %args.version, "updater: installed"),
        SwapOutcome::InstalledOldLeft(e) => tracing::warn!("updater: installed; the old version stays in the working folder: {e}"),
        SwapOutcome::RolledBack { step, why, relaunched } => {
            // Only when the old app is at the installed path (`run_swap`): the folder holds
            // nothing but the new copy.
            tracing::error!(?step, why, ?relaunched, "updater: not installed; the old version is back");
            let _ = remove_any(args.staged.parent().unwrap_or(&args.staged));
            let _ = std::fs::write(
                note_path(),
                format!("Stage Manager {} could not be installed ({why}), so you still have {}.", args.version, current()),
            );
        }
        SwapOutcome::RollbackFailed { step, why, undo } => {
            let old = args.staged.display();
            tracing::error!(?step, why, undo, %old, "updater: not installed, and the old version could not be put back; it is kept");
            let note = if undo == self_update::NEW_APP_STILL_RUNS {
                format!(
                    "Stage Manager {} did not answer after it was installed ({why}) and would not quit, so it was left in place. The previous version is kept at {old}. If something is wrong, quit it and download it again from GitHub.",
                    args.version
                )
            } else {
                format!(
                    "Stage Manager {} may not have installed cleanly ({why}). The previous version is kept at {old}. If something is wrong, download it again from GitHub.",
                    args.version
                )
            };
            let _ = std::fs::write(note_path(), note);
        }
        SwapOutcome::Stranded { step, why, old, new, relaunched } => {
            let at = |p: &Place| p.path(&args.target, &args.staged);
            let (old, new) = (at(old), at(new));
            tracing::error!(?step, why, old = %old.display(), new = %new.display(), ?relaunched, "updater: the exchange stopped part way; both copies are kept");
            let note = if new == args.target {
                format!(
                    "Stage Manager {} may not have installed cleanly ({why}). The previous version is kept at {}. If something is wrong, download it again from GitHub.",
                    args.version,
                    old.display()
                )
            } else {
                format!(
                    "Stage Manager {} could not be installed ({why}), and the previous version could not be put back: it is at {}. Move it to {}, or download Stage Manager again from GitHub.",
                    args.version,
                    old.display(),
                    args.target.display()
                )
            };
            let _ = std::fs::write(note_path(), note);
        }
    }
    if relaunch_failed(&outcome) {
        // The installed path held nothing to open (the app was moved since Install and
        // Relaunch): open the app wherever macOS knows it, never leave none running (review C12).
        match reopen_by_bundle_id() {
            Ok(()) => tracing::info!("updater: the app could not be opened at its path; opened it by its bundle ID"),
            Err(e) => tracing::error!("updater: the app could not be opened at its path nor by its bundle ID: {e}"),
        }
    }
    if outcome.keeps_work()
        && let Some(work) = args.staged.parent()
    {
        let _ = std::fs::write(work.join(self_update::KEEP_MARK), format!("{outcome:?}\n"));
    }
    if matches!(outcome, SwapOutcome::Installed | SwapOutcome::InstalledOldLeft(_)) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper starts last on the way out: after every window came back and the ctl
    /// socket is gone (so the new app does not take the old one for a running copy).
    #[test]
    fn the_helper_starts_on_the_apps_exit_path_after_the_socket_is_gone() {
        let main = include_str!("../main.rs");
        let exit = main.find("fn finish_exit").expect("finish_exit");
        let socket = main[exit..].find("ctl::remove_socket()").expect("socket removed") + exit;
        let helper = main[exit..].find("updater::launch_pending_helper()").expect("helper launched") + exit;
        assert!(socket < helper, "the socket goes first");
        assert!(main.contains("Some(\"--update-helper\") => stage_manager::platform::updater::run_helper"));
    }

    /// Release builds trust only a Developer ID signature and never read the test hooks.
    /// Review C3: a pid given to another process (another start time) is not the app.
    #[test]
    fn a_reused_pid_is_not_the_app() {
        assert!(same_start(Some((100, 5)), Some((100, 5))));
        assert!(!same_start(Some((100, 5)), Some((160, 0))), "another process at the pid");
        assert!(!same_start(Some((100, 5)), None), "unreadable: another user's process");
        assert!(same_start(None, Some((160, 0))), "nothing read at first: the pid decides");
        let me = std::process::id();
        assert!(app_still_runs(me, start_time(me as i32)));
        assert!(!app_still_runs(me, Some((1, 0))));
    }

    /// Review C12: a relaunch that failed at the installed path is made up for by bundle ID.
    #[test]
    fn a_failed_relaunch_is_seen() {
        let rolled = |relaunched| SwapOutcome::RolledBack { step: self_update::Step::Exchange, why: String::new(), relaunched };
        assert!(relaunch_failed(&rolled(Err("no such file".into()))));
        assert!(!relaunch_failed(&rolled(Ok(()))));
        assert!(!relaunch_failed(&SwapOutcome::Installed));
    }

    /// Review C11: kept disk images go after an hour; a young folder and others stay.
    #[test]
    fn old_manual_downloads_are_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join(format!("{DOWNLOAD_DIR_PREFIX}aaaaaa"));
        let young = dir.path().join(format!("{DOWNLOAD_DIR_PREFIX}bbbbbb"));
        let other = dir.path().join("something-else");
        for d in [&old, &young, &other] {
            std::fs::create_dir(d).expect("mkdir");
            std::fs::write(d.join(updates::DMG_NAME), "dmg").expect("write");
        }
        let hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        for d in [&old, &other] {
            std::fs::File::open(d).and_then(|f| f.set_modified(hours_ago)).expect("mtime");
        }
        assert_eq!(clean_old_downloads(dir.path(), MANUAL_KEEP), 1);
        assert!(!old.exists() && young.exists() && other.exists());
    }

    #[test]
    fn local_trust_is_for_test_builds_only() {
        assert_eq!(ALLOW_LOCAL_TRUST, cfg!(feature = "update-test"));
        let release_flags = include_str!("../../scripts/release.sh");
        assert!(!release_flags.contains("update-test"), "the release is never built with the test hooks");
    }

    /// Review 8C #1: the app itself is notarized and stapled before the DMG is built around
    /// it, so `spctl` accepts an update offline.
    #[test]
    fn the_release_staples_the_app_before_the_disk_image() {
        let script = include_str!("../../scripts/release.sh");
        let at = |needle: &str| script.find(needle).unwrap_or_else(|| panic!("release.sh: {needle}"));
        let app_notarized = at("notarize \"$APP_ZIP\"");
        let app_stapled = at("stapler staple \"$APP\"");
        let dmg_built = at("scripts/dmg.sh \"$APP\"");
        let dmg_notarized = at("notarize \"$DMG\"");
        let dmg_stapled = at("stapler staple \"$DMG\"");
        assert!(app_notarized < app_stapled && app_stapled < dmg_built && dmg_built < dmg_notarized && dmg_notarized < dmg_stapled);
    }
}
