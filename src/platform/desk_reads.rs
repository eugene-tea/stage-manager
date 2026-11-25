//! Reading where the windows are: the window server's Desktops, under every window engine
//! (round corner-desktops; docs/ARCHITECTURE.md, "Layers").
//!
//! Two read-only calls of SkyLight, loaded at run time, nothing linked:
//! `SLSCopyManagedDisplaySpaces` (each screen's Space shown and its Desktops,
//! [`ScreenDesks`]) and `SLSCopySpacesForWindows` (the Spaces a window is on), on the
//! connection `SLSMainConnectionID` names. Their signatures and the reads themselves are
//! [`super::spaces::ffi`]'s, the same the Spaces engine uses for its own book-keeping; this
//! module loads those three names alone, so the Corner engine loads no call that moves or
//! hides a window, and no launch probe is needed for it.
//!
//! What the controller decides by goes through [`desktops`] and [`spaces_of`], which answer
//! only while the reads work ([`crate::engine::desk_reads`]: the calls there, and what they
//! say agreeing with the window list, checked by the controller's look,
//! `Controller::check_reads`); `None` otherwise, and the controller goes by the windows on
//! screen as before the reads. `STAGE_MANAGER_DESKTOPS_GUESS` (honoured only with
//! `STAGE_MANAGER_ONLY`, a test run) forces that fallback. [`status`] says which way it
//! runs and why (`ctl state`'s `desks.reads`).
//!
//! In a unit test the reads are a fake desk's ([`super::fake`]); with no desk nothing is
//! read of the Mac the test runs on.

use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use serde_json::{Value, json};

use super::clock;
use super::spaces::ffi::{self, FnCopyByCid, FnCopySpacesForWindows, FnMain, SKYLIGHT, ScreenDesks};
use crate::engine::desk_reads::{Change, Health, Verdict};

/// Set (with `STAGE_MANAGER_ONLY`): the reads are off, Desktops go by the windows on screen.
pub const FORCE_GUESS_ENV: &str = "STAGE_MANAGER_DESKTOPS_GUESS";

/// The calls, loaded once.
struct Calls {
    cid: i32,
    copy_managed: FnCopyByCid,
    spaces_for_windows: FnCopySpacesForWindows,
}

static CALLS: OnceLock<Result<Calls, String>> = OnceLock::new();

/// The calls, or why there are none (said once in the log).
fn calls() -> Result<&'static Calls, &'static str> {
    CALLS
        .get_or_init(|| {
            let opened = open();
            match &opened {
                Ok(c) => tracing::info!(connection = c.cid, "desktops: the window server's Desktops can be read (the listing, a window's Spaces)"),
                Err(why) => tracing::warn!("desktops: the window server's Desktops cannot be read: {why}; Desktops go by the windows on screen"),
            }
            opened
        })
        .as_ref()
        .map_err(String::as_str)
}

fn open() -> Result<Calls, String> {
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    if h.is_null() {
        return Err("SkyLight did not load".into());
    }
    let mut names = Vec::new();
    // SAFETY (each `load`): the types follow the public signatures `spaces::ffi` names.
    let main: Option<FnMain> = unsafe { ffi::load(h, c"SLSMainConnectionID", &mut names) };
    let copy_managed: Option<FnCopyByCid> = unsafe { ffi::load(h, c"SLSCopyManagedDisplaySpaces", &mut names) };
    let spaces_for_windows: Option<FnCopySpacesForWindows> = unsafe { ffi::load(h, c"SLSCopySpacesForWindows", &mut names) };
    let (Some(main), Some(copy_managed), Some(spaces_for_windows)) = (main, copy_managed, spaces_for_windows) else {
        let missing: Vec<&str> = names.iter().filter(|(_, found)| !found).map(|(n, _)| *n).collect();
        return Err(format!("missing on this macOS: {}", missing.join(", ")));
    };
    // SAFETY: no arguments; this process's connection id.
    let cid = unsafe { main() };
    Ok(Calls { cid, copy_managed, spaces_for_windows })
}

/// True when a test run asked for the fallback ([`FORCE_GUESS_ENV`] with `STAGE_MANAGER_ONLY`).
fn forced_off() -> bool {
    static FORCED: LazyLock<bool> = LazyLock::new(|| {
        let forced = forced_by(std::env::var_os(FORCE_GUESS_ENV).is_some(), std::env::var_os("STAGE_MANAGER_ONLY").is_some());
        if forced {
            tracing::info!("desktops: {FORCE_GUESS_ENV} set (a test run): the window server's Desktops are not read");
        }
        forced
    });
    *FORCED
}

/// The test switch counts only in a test run (`only`: `STAGE_MANAGER_ONLY` set).
fn forced_by(asked: bool, only: bool) -> bool {
    asked && only
}

/// Why nothing can be read in this run, if so: the test switch, or the calls missing.
fn unavailable() -> Option<&'static str> {
    #[cfg(test)]
    if let Some(on) = super::fake::with(|d| d.desk_reads) {
        return (!on).then_some("the calls do not answer on this desk");
    }
    if cfg!(test) {
        return Some("a unit test with no desk reads nothing");
    }
    if forced_off() {
        return Some("forced off by a test run");
    }
    calls().err()
}

static HEALTH: Mutex<Health> = Mutex::new(Health::new());

/// Runs `f` on the reads' health: this process's, or a fake desk's own.
fn with_health<T>(f: impl FnOnce(&mut Health) -> T) -> T {
    #[cfg(test)]
    let f = {
        let mut f = Some(f);
        if let Some(out) = super::fake::with(|d| f.take().map(|f| f(&mut d.reads_health))).flatten() {
            return out;
        }
        let Some(f) = f else { unreachable!("a desk that answered took the call") };
        f
    };
    f(&mut HEALTH.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// True while the reads decide: there, and not resting.
pub fn decides() -> bool {
    unavailable().is_none() && with_health(|h| h.decides(clock::now()))
}

/// Every screen's entry of the listing, read now, whatever the check says: for the check
/// itself. `None` when nothing can be read in this run.
pub(crate) fn raw_desktops() -> Option<Vec<ScreenDesks>> {
    if unavailable().is_some() {
        return None;
    }
    #[cfg(test)]
    if let Some(out) = super::fake::with(|d| d.sky_displays()) {
        return Some(out);
    }
    let c = calls().ok()?;
    Some(ffi::read_displays(c.copy_managed, c.cid))
}

/// The Spaces the window server lists windows `ids` on now, whatever the check says.
pub(crate) fn raw_spaces_of(ids: &[u32]) -> Option<Vec<u64>> {
    if unavailable().is_some() {
        return None;
    }
    #[cfg(test)]
    if let Some(out) = super::fake::with(|d| d.sky_spaces_of(ids)) {
        return Some(out);
    }
    let c = calls().ok()?;
    Some(ffi::read_spaces_of(c.spaces_for_windows, c.cid, ids))
}

/// The window server's listing of every screen's Desktops, read now; `None` while the reads
/// do not decide (module docs).
pub fn desktops() -> Option<Vec<ScreenDesks>> {
    if !decides() {
        return None;
    }
    raw_desktops()
}

/// The Spaces the window server lists window `id` on now; `None` while the reads do not
/// decide.
pub fn spaces_of(id: u32) -> Option<Vec<u64>> {
    if !decides() {
        return None;
    }
    raw_spaces_of(&[id])
}

/// True when the check's look is due at `now` ([`Health::due`]); never when nothing can be
/// read.
pub fn check_due(now: Instant) -> bool {
    unavailable().is_none() && with_health(|h| h.due(now))
}

/// Something the listing said looks doubtful: the check's next look is due ([`Health::suspect`]).
pub fn suspect() {
    with_health(Health::suspect);
}

/// The check's look said `verdict` at `now`: logged when the reads rest or decide again.
/// True when they rest from now.
pub fn checked(verdict: Verdict, now: Instant) -> bool {
    match with_health(|h| h.looked(verdict, now)) {
        Some(Change::Rested { why, first: true }) => {
            tracing::warn!(
                why,
                rest_s = crate::engine::desk_reads::OFF_FOR.as_secs(),
                "desktops: the window server's listing disagrees with the windows on screen: its reads rest, Desktops go by the windows on screen"
            );
            true
        }
        Some(Change::Rested { why, first: false }) => {
            tracing::debug!(why, "desktops: the window server's listing still disagrees with the windows on screen: its reads rest again");
            true
        }
        Some(Change::Back) => {
            tracing::info!("desktops: the window server's listing agrees with the windows on screen again: it decides again");
            false
        }
        None => false,
    }
}

/// Which way the Desktops are read and why, for `ctl state` (`desks.reads`).
pub fn status() -> Value {
    let now = clock::now();
    let off = unavailable();
    let (state, rest_s, agreed, disagreed, rests, last) = with_health(|h| {
        let (word, rest) = h.word(now);
        (word, rest, h.agreed, h.disagreed, h.rests, h.last_why)
    });
    let decides = off.is_none() && state == "working";
    json!({
        "way": if decides { "window server" } else { "windows on screen" },
        "state": if off.is_some() { "off" } else { state },
        "why": off.or((state != "working").then_some("the listing disagreed with the windows on screen")),
        "rest_s": rest_s,
        "checks": { "agreed": agreed, "disagreed": disagreed, "rests": rests, "last_disagreement": last },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_counts_only_in_a_test_run() {
        assert!(forced_by(true, true));
        assert!(!forced_by(true, false), "never on a user's copy");
        assert!(!forced_by(false, true));
    }

    #[test]
    fn a_unit_test_with_no_desk_reads_nothing_of_this_mac() {
        assert_eq!(desktops(), None);
        assert_eq!(spaces_of(1), None);
        assert_eq!(status()["way"], "windows on screen");
    }
}
