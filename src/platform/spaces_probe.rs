//! `stage-manager probe-spaces …`: the Spaces spike (1.9, S0). Measures whether this process
//! can run stages as hidden window-server Spaces: a Space it creates, a fixture app's windows
//! moved into it, the Space hidden and shown, its transform stepped, a new window placed in it
//! before it shows, and what is left after a `kill -9`.
//!
//! Every call is loaded at run time: SkyLight's C functions with `dlopen`/`dlsym`, its bridged
//! operations (`SLSBridged…Operation`) as Objective-C classes looked up by name and sent
//! through `objc_msgSend` (also found with `dlsym`). Nothing new is linked. Two routes:
//! `classic` (`SLSSpaceCreate`, `SLSShowSpaces`, …) and `bridged` (the operation classes,
//! performed with `-performWithWMBridgeDelegate`, which needs AppKit loaded: the probe makes
//! itself an accessory `NSApplication` first). The signatures come from public sources only;
//! the spike's notes list each with its source.
//!
//! Refuses any bundle id but a fixture's (`dev.eugene.stagemanager.fixture…`) and touches only
//! that app's layer-0 windows. Every Space it makes is written to
//! `/tmp/sm-spaces-probe/spaces.txt` before windows go into it; `probe-spaces cleanup`
//! brings every fixture window back to the Desktop shown and destroys those Spaces.
//!
//! Routes per step: `--route R` (classic or bridged, default bridged) for create, show, hide,
//! level, transform and destroy; `--create R` and `--add R` override it for the create and
//! for putting windows into a Space. What the spike found (macOS 27.2): only the bridged add
//! moves another app's window, and a classic Space dies with its creator, so `--route classic
//! --add bridged` is the combination that brings windows back by itself after a `kill -9`.
//!
//! Subcommands (one JSON object on stdout each, `hide` and `place-new` one line per event):
//! - `read [BUNDLE]`: symbols and classes found, the Desktops, every Space, the recorded
//!   Spaces, the fixture's windows and their Spaces.
//! - `cycle BUNDLE [--back move|show] [--rounds N] [--level L] [--managed]`: create, then N
//!   rounds of hide and show (back to the Desktop, or the Space shown), timed to the window
//!   list with this process's CPU per call, then clean up.
//! - `hide BUNDLE [--hold S] [--show-space]`: hide the windows in a new Space (then show that
//!   Space) and hold S seconds (for a `kill -9` or the Mission Control checks), then bring
//!   them back.
//! - `transform BUNDLE [--steps N] [--scale K] [--shown] [--pace-ms MS] [--hold-ms MS]
//!   [no-shots]`: the windows in a new Space, its transform stepped from identity to a scale
//!   K, each step timed, read back and checked in the window list; screenshots of the halfway
//!   step and the last.
//! - `affine BUNDLE [--m "a,b,c,d,tx,ty;…"] [--fly N] [--per-window] [--only I]
//!   [--pace-ms MS] [--hold-ms MS] [--rest-ms MS] [no-shots]` (S4): the windows in one shown Space (or one each), then
//!   either each matrix of `--m` set, held, read back, listed and photographed (the exact
//!   mapping), or `--fly N` steps of a shrink toward the screen's left edge and back, paced,
//!   each step's calls timed (what a motion through the transform costs). What it found
//!   (macOS 27.2, 11-14): the matrix is each window's own, about that window's top-left
//!   corner: a point `q` of the window (from its corner `o`) is drawn at `o + M⁻¹(q − t)`,
//!   wherever the window is; scales, shears and moves all apply, there is no perspective.
//! - `place-new BUNDLE [--for S]`: a hidden Space waits; every fixture window that appears in
//!   the next S seconds goes into it as soon as it is listed, on screen or not yet.
//! - `watch BUNDLE [--for S] [--every MS]`: prints each change of the fixture windows'
//!   on-screen flag and Spaces (after a `kill -9` of a holder).
//! - `space-op show|hide|destroy SPACE`: one call on a recorded Space (an orphan).
//! - `key mission-control|app-windows|escape|cmd-tab|left|right`: posts that shortcut.
//! - `dock-click BUNDLE`: presses the fixture's Dock item through Accessibility.
//! - `engine BUNDLE [--rounds N] [--hold-ms MS]`: the Spaces engine itself
//!   ([`super::spaces::engine`]) hides and shows the fixture's windows N times from a thread
//!   that is not the main one (as the app's WM thread does), each step read back and checked
//!   against the window list, then its bring-back; the engine's counts.
//! - `launch-probe [--rounds N]`: the app's launch probe, N times, each verdict and its time.
//! - `sources APP-PATH BUNDLE [--rounds N] [--events A-B] [--args …]`: how early each source
//!   tells of a newly opened app's first window ([`super::new_window_probe`]); reads only.
//! - `overlap OLD-BUNDLE APP-PATH NEW-BUNDLE [--for MS] [--args …]`: opens the app while the
//!   old app's windows show and samples the on-screen list every ms: any sample with both
//!   ([`super::new_window_probe::overlap`]); reads only.
//! - `settle --in "ID,X,Y,W,H;…" [--out …] [--for MS] [--every MS] [--overlay PID] --then CMD ARG…`: runs the
//!   command (a switch) while these windows' descriptions are read every ms: when each
//!   incoming window is at its frame for good, each outgoing one gone from its own
//!   ([`super::settle_probe`], round sm-measure); reads only.
//! - `pixels --rect X,Y,W,H --windows ID,… [--every MS] [--for MS] --then CMD ARG…` (round
//!   landing-order): runs the command (a switch) while the screen's region is captured every
//!   few ms with only these fixture windows in it ([`super::pixels_probe`]); reads only.
//! - `capture BUNDLE [--rounds N] [hidden|cached file=PATH] [fast]` (S4b, S6b): a switch's
//!   pictures taken at the switch, through the app's path, timed on screen and in a stage
//!   Space not shown; `fast` through the window server's call alone ([`capture`]'s docs).
//! - `notices --ids ID,… [--events A-B] [--for MS]`: the window server's notices this process
//!   hears for windows that exist, asked for by id (`SLSRequestNotificationsForWindows`), while
//!   they are closed, moved or ordered out by hand ([`super::new_window_probe::notices`],
//!   round sm-tick: which could stir the WM tick); reads only.
//! - `cleanup [BUNDLE]`: every fixture window back to the Desktop shown, every recorded Space
//!   shown and destroyed (both routes), the record emptied.
//! - `dock-notices`, `space-flip`, `names`, `indicator` (round r0-probes), `cgs` (round
//!   r2-probes2): the five live probes of [`system`], plain tables and verdicts, through
//!   `scripts/probe_system.py`.


use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_core_foundation::{CFRunLoop, CGAffineTransform, kCFRunLoopDefaultMode};
use objc2_foundation::{MainThreadMarker, NSString};
use serde_json::{Value, json};

use super::park_probe::FIXTURE_PREFIX;
use super::spaces::ffi::{ALL_SPACES_MASK, Route, Sky};
use super::windows::{self, CgWindow};

#[path = "spaces_probe_capture.rs"]
mod capture;
#[path = "system_probe/mod.rs"]
mod system;

/// Where the probe keeps its record of the Spaces it made, and screenshots.
pub const PROBE_DIR: &str = "/tmp/sm-spaces-probe";


/// Parsed options shared by the subcommands.
struct Opts {
    bundle: Option<String>,
    route: Route,
    create_route: Option<Route>,
    add_route: Option<Route>,
    show_space: bool,
    back_show: bool,
    rounds: usize,
    level: Option<i32>,
    managed: bool,
    hold: f64,
    steps: usize,
    scale: f64,
    shown: bool,
    pace_ms: f64,
    hold_ms: u64,
    every_ms: u64,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        bundle: None,
        route: Route::Bridged,
        create_route: None,
        add_route: None,
        show_space: false,
        back_show: false,
        rounds: 5,
        level: None,
        managed: false,
        hold: 20.0,
        steps: 120,
        scale: 0.5,
        shown: false,
        pace_ms: 0.0,
        hold_ms: 300,
        every_ms: 2,
        words: Vec::new(),
    };
    let mut it = args.iter();
    let num = |v: Option<&String>, what: &str| v.and_then(|s| s.parse::<f64>().ok()).ok_or(format!("{what} takes a number"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--create" => {
                o.create_route = match it.next().map(String::as_str) {
                    Some("classic") => Some(Route::Classic),
                    Some("bridged") => Some(Route::Bridged),
                    _ => return Err("--create takes classic or bridged".into()),
                }
            }
            "--show-space" => o.show_space = true,
            "--add" => {
                o.add_route = match it.next().map(String::as_str) {
                    Some("classic") => Some(Route::Classic),
                    Some("bridged") => Some(Route::Bridged),
                    _ => return Err("--add takes classic or bridged".into()),
                }
            }
            "--route" => {
                o.route = match it.next().map(String::as_str) {
                    Some("classic") => Route::Classic,
                    Some("bridged") => Route::Bridged,
                    _ => return Err("--route takes classic or bridged".into()),
                }
            }
            "--back" => o.back_show = it.next().map(String::as_str) == Some("show"),
            "--rounds" => o.rounds = num(it.next(), "--rounds")? as usize,
            "--level" => o.level = Some(num(it.next(), "--level")? as i32),
            "--managed" => o.managed = true,
            "--hold" | "--for" => o.hold = num(it.next(), "--hold")?,
            "--steps" => o.steps = num(it.next(), "--steps")?.max(1.0) as usize,
            "--scale" => o.scale = num(it.next(), "--scale")?,
            "--shown" => o.shown = true,
            "--pace-ms" => o.pace_ms = num(it.next(), "--pace-ms")?,
            "--hold-ms" => o.hold_ms = num(it.next(), "--hold-ms")? as u64,
            "--every" => o.every_ms = num(it.next(), "--every")?.max(1.0) as u64,
            b if b.starts_with("dev.") && o.bundle.is_none() => o.bundle = Some(b.to_string()),
            w => o.words.push(w.to_string()),
        }
    }
    if let Some(b) = &o.bundle
        && !b.starts_with(FIXTURE_PREFIX)
    {
        return Err("probe-spaces moves windows: only a fixture app's (dev.eugene.stagemanager.fixture…)".into());
    }
    Ok(o)
}

fn print(v: Value) {
    println!("{v}");
    let _ = std::io::stdout().flush();
}

fn fail(code: u8, error: &str) -> ExitCode {
    print(json!({ "ok": false, "error": error }));
    ExitCode::from(code)
}

/// Runs the probe (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let Some((cmd, rest)) = args.split_first() else {
        return fail(2, "usage: stage-manager probe-spaces read|cycle|hide|transform|affine|place-new|watch|engine|capture|launch-probe|key|dock-click|cleanup|sources|overlap|settle|pixels|notices|dock-notices|space-flip|names|indicator|cgs …");
    };
    // Read only (no window moves): any app, their own arguments.
    if cmd == "sources" {
        return super::new_window_probe::run(rest);
    }
    if cmd == "overlap" {
        return super::new_window_probe::overlap(rest);
    }
    if cmd == "settle" {
        return super::settle_probe::run(rest);
    }
    if cmd == "pixels" {
        return super::pixels_probe::run(rest);
    }
    if cmd == "notices" {
        return super::new_window_probe::notices(rest);
    }
    if let Some(code) = system::run(cmd, rest) {
        return code;
    }
    let o = match parse(rest) {
        Ok(o) => o,
        Err(e) => return fail(2, &e),
    };
    let Some(mtm) = MainThreadMarker::new() else { return fail(4, "not on the main thread") };
    // The bridge's delegate is AppKit's window coordinator: load AppKit as an accessory app
    // (no Dock icon, no menu bar).
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let sky = match Sky::open() {
        Ok(s) => s,
        Err(e) => return fail(3, &e),
    };
    let _ = std::fs::create_dir_all(PROBE_DIR);
    let need_bundle = || o.bundle.clone().ok_or("this subcommand takes a fixture bundle id".to_string());
    let result = match cmd.as_str() {
        "read" => Ok(read(&sky, o.bundle.as_deref())),
        "cycle" => need_bundle().and_then(|b| cycle(&sky, &b, &o)),
        "hide" => need_bundle().and_then(|b| hide_hold(&sky, &b, &o)),
        "transform" => need_bundle().and_then(|b| transform(&sky, &b, &o)),
        "affine" => need_bundle().and_then(|b| affine(&sky, &b, &o)),
        "place-new" => need_bundle().and_then(|b| place_new(&sky, &b, &o)),
        "watch" => need_bundle().map(|b| watch(&sky, &b, &o)),
        "key" => key(&o),
        "dock-click" => need_bundle().and_then(|b| dock_click(&b)),
        "cleanup" => Ok(cleanup(&sky, o.bundle.as_deref())),
        "space-op" => space_op(&sky, &o),
        "engine" => need_bundle().and_then(|b| engine_rounds(&sky, &b, &o)),
        "launch-probe" => Ok(launch_probe(mtm, &o)),
        "capture" => {
            let scale = objc2_app_kit::NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor());
            need_bundle().and_then(|b| capture::run(&sky, &b, &o, scale))
        }
        _ => Err(format!("unknown subcommand {cmd}")),
    };
    match result {
        Ok(v) => {
            print(v);
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Whatever failed, nothing of the fixture's stays hidden.
            let c = cleanup(&sky, o.bundle.as_deref());
            print(json!({ "ok": false, "error": e, "cleanup": c }));
            ExitCode::FAILURE
        }
    }
}

/// The app's launch probe ([`super::spaces::probe`]) run `--rounds N` times in this process,
/// each verdict with its time: what every launch decides by, without launching the app.
fn launch_probe(mtm: MainThreadMarker, o: &Opts) -> Value {
    let runs: Vec<Value> = (0..o.rounds).map(|_| json!(super::spaces::probe::run(mtm))).collect();
    json!({ "ok": true, "runs": runs })
}

/// The Spaces engine driven as the app drives it, off the main thread (see the module docs).
fn engine_rounds(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let pids = fixture_pids(bundle);
    let ids = ids_of(&fixture_windows(sky, &pids));
    if ids.is_empty() {
        return Err("the fixture has no window".into());
    }
    let (rounds, hold) = (o.rounds, Duration::from_millis(o.hold_ms));
    let worker = std::thread::spawn(move || {
        let engine = super::spaces::engine::shared();
        let mut out = Vec::new();
        for round in 0..rounds {
            let t0 = Instant::now();
            let hidden: Vec<bool> = ids.iter().map(|id| engine.hide_window(*id)).collect();
            let hide_ms = ms(t0);
            let (off_ms, _) = wait_onscreen(&ids, false, Duration::from_millis(500));
            let here = engine.present_now().len();
            std::thread::sleep(hold);
            let t1 = Instant::now();
            let shown: Vec<bool> = ids.iter().map(|id| engine.show_window(*id)).collect();
            let show_ms = ms(t1);
            let (on_ms, _) = wait_onscreen(&ids, true, Duration::from_millis(500));
            let spaces: Vec<Vec<u64>> = ids.iter().map(|id| engine.spaces_of(*id)).collect();
            std::thread::sleep(hold);
            let later: Vec<Vec<u64>> = ids.iter().map(|id| engine.spaces_of(*id)).collect();
            out.push(json!({ "round": round, "spaces_shown": spaces, "spaces_later": later, "hidden": hidden, "hide_ms": hide_ms, "off_screen_ms": off_ms, "present": here, "shown": shown, "show_ms": show_ms, "on_screen_ms": on_ms }));
        }
        // Left hidden once more, for the bring-back to find by the window server's list.
        let left: Vec<bool> = ids.iter().map(|id| engine.hide_window(*id)).collect();
        let swept = engine.sweep("probe", true);
        let (back_ms, list) = wait_onscreen(&ids, true, Duration::from_millis(500));
        json!({
            "rounds": out,
            "left_hidden": left,
            "sweep": { "checked": swept.checked, "restored": swept.restored, "failed": swept.failed, "on_screen_ms": back_ms },
            "windows": frames(&list),
            "engine": engine.status(),
        })
    });
    // The main run loop turns meanwhile, as the app's does.
    while !worker.is_finished() {
        pump(Duration::from_millis(5));
    }
    worker.join().map_err(|_| "the engine's thread panicked".to_string())
}

/// The fixture's processes.
fn fixture_pids(bundle: &str) -> Vec<i32> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle))
        .iter()
        .map(|a| a.processIdentifier())
        .collect()
}

/// Every fixture process's pid (any fixture bundle id).
fn all_fixture_pids() -> Vec<i32> {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|a| a.bundleIdentifier().is_some_and(|b| b.to_string().starts_with(FIXTURE_PREFIX)))
        .map(|a| a.processIdentifier())
        .collect()
}

/// The fixture's layer-0 windows that are on some Space, as the window server lists them
/// (the fixture's never-shown helper windows are on none and are left out).
fn fixture_windows(sky: &Sky, pids: &[i32]) -> Vec<CgWindow> {
    windows::list_windows()
        .into_iter()
        .filter(|w| pids.contains(&w.pid) && w.layer == 0 && (w.bounds.h > 50.0 || !sky.spaces_of(&[w.id]).is_empty()))
        .collect()
}

fn ids_of(list: &[CgWindow]) -> Vec<u32> {
    list.iter().map(|w| w.id).collect()
}

/// Lets the run loop turn for `d` (the bridge may answer on it).
fn pump(d: Duration) {
    // SAFETY: the framework's own constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, d.as_secs_f64(), false);
}

/// Polls the window list until every window's on-screen flag is `want`; the ms it took.
fn wait_onscreen(ids: &[u32], want: bool, limit: Duration) -> (Option<f64>, Vec<CgWindow>) {
    let t0 = Instant::now();
    loop {
        let list = windows::describe(ids);
        if list.len() == ids.len() && list.iter().all(|w| w.onscreen == want) {
            return (Some(ms(t0)), list);
        }
        if t0.elapsed() > limit {
            return (None, list);
        }
        pump(Duration::from_micros(250));
    }
}

fn ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// This process's CPU time (user + system), in ms.
fn cpu_ms() -> f64 {
    // SAFETY: an all-zero rusage is a valid out value.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF with a writable struct.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 * 1e3 + v.tv_usec as f64 / 1e3;
    t(u.ru_utime) + t(u.ru_stime)
}

fn frames(list: &[CgWindow]) -> Value {
    json!(list.iter().map(|w| json!({ "id": w.id, "onscreen": w.onscreen, "bounds": w.bounds, "alpha": w.alpha })).collect::<Vec<_>>())
}

// The record of the Spaces the probe made.

fn record_path() -> std::path::PathBuf {
    std::path::Path::new(PROBE_DIR).join("spaces.txt")
}

/// The Spaces the probe made in this window server session ([`recorded_in`]).
fn recorded() -> Vec<u64> {
    recorded_in(&std::fs::read_to_string(record_path()).unwrap_or_default(), super::session::window_session())
}

/// The Space ids of the record `text` made in window server session `session`. A line of
/// another session (or of a probe that wrote none) names an id that may be anyone's now: the
/// file outlives a logout (`/tmp` is cleared at a restart), the window server's ids start
/// over, and cleanup would show and end a Desktop or a running app's Space (review A-14).
fn recorded_in(text: &str, session: Option<&str>) -> Vec<u64> {
    let Some(session) = session else { return Vec::new() };
    text.lines()
        .filter(|l| l.split_once("\tsession=").is_some_and(|(_, s)| s == session))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

fn record(sid: u64, route: Route) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(record_path()) {
        let session = super::session::window_session().unwrap_or("?");
        let _ = writeln!(f, "{sid} {} pid={}\tsession={session}", route.name(), std::process::id());
    }
}

fn unrecord(sid: u64) {
    let keep: Vec<String> = std::fs::read_to_string(record_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_whitespace().next().and_then(|s| s.parse::<u64>().ok()) != Some(sid))
        .map(String::from)
        .collect();
    let _ = std::fs::write(record_path(), if keep.is_empty() { String::new() } else { keep.join("\n") + "\n" });
}

/// A new Space by `route`, recorded, at `level` when given.
fn new_space(sky: &Sky, o: &Opts) -> Result<(u64, Value), String> {
    let t0 = Instant::now();
    let sid = sky.create(o.create_route.unwrap_or(o.route), o.managed)?;
    let create_ms = ms(t0);
    if sid == 0 {
        return Err("the create call returned Space 0".into());
    }
    record(sid, o.create_route.unwrap_or(o.route));
    let level = o.level.map(|l| sky.set_level(o.route, sid, l));
    let info = json!({
        "space": sid,
        "create_ms": create_ms,
        "type": sky.space_type(sid),
        "listed_all": sky.all_spaces(0xf).contains(&sid),
        "managed": sky.displays().iter().any(|d| d.2.iter().any(|s| s.0 == sid)),
        "level": level.map(|r| format!("{r:?}")),
    });
    Ok((sid, info))
}

/// Windows back to the Desktop shown, the Space shown and destroyed, both routes when needed.
fn tear_down(sky: &Sky, sid: u64, ids: &[u32], route: Route) -> Value {
    let (current, _) = sky.current();
    let mut steps = Vec::new();
    for r in [route, other(route)] {
        let still: Vec<u32> = windows::describe(ids).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
        if still.is_empty() {
            break;
        }
        steps.push(json!({ "route": r.name(), "move_back": format!("{:?}", sky.move_managed(r, &still, current)) }));
        let _ = wait_onscreen(&still, true, Duration::from_millis(500));
        let still: Vec<u32> = windows::describe(ids).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
        if !still.is_empty() {
            steps.push(json!({ "route": r.name(), "add_back": format!("{:?}", sky.add_remove(r, current, &still)) }));
            let _ = wait_onscreen(&still, true, Duration::from_millis(500));
        }
    }
    let _ = sky.show_hide(route, &[sid], true);
    let mut destroyed = false;
    for r in [route, other(route)] {
        let d = sky.destroy(r, sid);
        pump(Duration::from_millis(150));
        destroyed = !sky.all_spaces(0xf).contains(&sid);
        steps.push(json!({ "route": r.name(), "destroy": format!("{d:?}"), "gone": destroyed }));
        if destroyed {
            break;
        }
    }
    if destroyed {
        unrecord(sid);
    }
    let after = windows::describe(ids);
    json!({ "steps": steps, "destroyed": destroyed, "all_onscreen": after.iter().all(|w| w.onscreen), "frames": frames(&after) })
}

fn other(r: Route) -> Route {
    match r {
        Route::Classic => Route::Bridged,
        Route::Bridged => Route::Classic,
    }
}

fn read(sky: &Sky, bundle: Option<&str>) -> Value {
    let classes = [
        c"SLSBridgedSpaceCreateOperation",
        c"SLSBridgedSpaceDestroyOperation",
        c"SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation",
        c"SLSBridgedHideSpacesOperation",
        c"SLSBridgedShowSpacesOperation",
        c"SLSBridgedSpaceSetTransformOperation",
        c"SLSBridgedSpaceSetAbsoluteLevelOperation",
        c"SLSBridgedMoveWindowsToManagedSpaceOperation",
    ];
    let windows = bundle.map(|b| {
        let list = fixture_windows(sky, &fixture_pids(b));
        list.iter().map(|w| json!({ "id": w.id, "onscreen": w.onscreen, "bounds": w.bounds, "spaces": sky.spaces_of(&[w.id]) })).collect::<Vec<_>>()
    });
    json!({
        "ok": true,
        "cid": sky.cid,
        "all_spaces": sky.all_spaces(ALL_SPACES_MASK),
        "symbols": sky.names.iter().map(|(n, f)| json!({ "name": n, "found": f })).collect::<Vec<_>>(),
        "classes": classes.iter().map(|c| json!({ "name": c.to_str().unwrap_or("?"), "found": Sky::class(c).is_some() })).collect::<Vec<_>>(),
        "active": sky.active.map(|f| unsafe { f(sky.cid) }), // SAFETY: plain query.
        "displays": sky.displays().iter().map(|(u, c, s)| json!({ "uuid": u, "current": c, "spaces": s })).collect::<Vec<_>>(),
        "recorded": recorded().iter().map(|&s| json!({ "space": s, "type": sky.space_type(s), "exists": sky.all_spaces(0xf).contains(&s) })).collect::<Vec<_>>(),
        "windows": windows,
    })
}

/// Q1 and Q6: hide and show rounds, timed to the window list.
fn cycle(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    if list.iter().any(|w| !w.onscreen) {
        return Err("a fixture window is not on screen before the run".into());
    }
    let before = frames(&list);
    let (current, _) = sky.current();
    let (sid, info) = new_space(sky, o)?;
    let mut rounds = Vec::new();
    let mut hidden_first = Value::Null;
    for i in 0..o.rounds {
        // Hide: into our Space (and the Space hidden; a new Space starts hidden).
        let c0 = cpu_ms();
        let t0 = Instant::now();
        let r = if o.back_show && i > 0 { sky.show_hide(o.route, &[sid], false) } else { sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids) };
        let call_ms = ms(t0);
        let call_cpu = cpu_ms() - c0;
        let (hide_ms, hidden) = wait_onscreen(&ids, false, Duration::from_secs(2));
        let spaces_hidden = sky.spaces_of(&ids);
        if i == 0 {
            hidden_first = json!({ "frames": frames(&hidden), "spaces": spaces_hidden, "listed": hidden.len() });
        }
        pump(Duration::from_millis(100));
        // Show: back to the Desktop, or our Space shown.
        let c1 = cpu_ms();
        let t1 = Instant::now();
        let s = if o.back_show { sky.show_hide(o.route, &[sid], true) } else { sky.add_remove(o.add_route.unwrap_or(o.route), current, &ids) };
        let show_call_ms = ms(t1);
        let show_cpu = cpu_ms() - c1;
        let (show_ms, shown) = wait_onscreen(&ids, true, Duration::from_secs(2));
        let kept = shown.iter().all(|w| list.iter().any(|b| b.id == w.id && b.bounds == w.bounds));
        rounds.push(json!({
            "hide": { "result": format!("{r:?}"), "call_ms": call_ms, "cpu_ms": call_cpu, "to_list_ms": hide_ms },
            "show": { "result": format!("{s:?}"), "call_ms": show_call_ms, "cpu_ms": show_cpu, "to_list_ms": show_ms, "frames_kept": kept },
            "spaces_shown": sky.spaces_of(&ids),
        }));
        if hide_ms.is_none() || show_ms.is_none() {
            break;
        }
        pump(Duration::from_millis(100));
    }
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({
        "ok": true, "route": o.route.name(), "back": if o.back_show { "show" } else { "move" },
        "desktop": current, "space": info, "windows": ids, "before": before, "hidden_first": hidden_first,
        "rounds": rounds, "teardown": down,
    }))
}

/// Q2 and Q5: hides the windows and holds.
fn hide_hold(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    let (sid, info) = new_space(sky, o)?;
    let t0 = Instant::now();
    let r = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids);
    let (hide_ms, hidden) = wait_onscreen(&ids, false, Duration::from_secs(2));
    print(json!({
        "event": "hidden", "pid": std::process::id(), "space": info, "windows": ids, "call": format!("{r:?}"),
        "call_ms": ms(t0), "to_list_ms": hide_ms, "frames": frames(&hidden), "spaces": sky.spaces_of(&ids),
    }));
    let mut hidden = hidden;
    if o.show_space {
        let t1 = Instant::now();
        let r = sky.show_hide(o.route, &[sid], true);
        let (t, now) = wait_onscreen(&ids, true, Duration::from_secs(2));
        print(json!({ "event": "space shown", "call": format!("{r:?}"), "call_ms": ms(t1), "to_list_ms": t, "frames": frames(&now) }));
        hidden = now;
    }
    let until = Instant::now() + Duration::from_secs_f64(o.hold);
    let mut last: Vec<(u32, bool)> = hidden.iter().map(|w| (w.id, w.onscreen)).collect();
    while Instant::now() < until {
        pump(Duration::from_millis(20));
        let now = windows::describe(&ids);
        let state: Vec<(u32, bool)> = now.iter().map(|w| (w.id, w.onscreen)).collect();
        if state != last {
            print(json!({ "event": "changed", "t_ms": ms(t0), "frames": frames(&now), "spaces": sky.spaces_of(&ids) }));
            last = state;
        }
    }
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({ "ok": true, "event": "done", "teardown": down }))
}

/// Q3: the transform stepped on a Space holding the live windows.
fn transform(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    let (sid, info) = new_space(sky, o)?;
    let add = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids);
    let _ = wait_onscreen(&ids, false, Duration::from_secs(1));
    let shown = if o.shown {
        let r = sky.show_hide(o.route, &[sid], true);
        let (t, _) = wait_onscreen(&ids, true, Duration::from_secs(1));
        json!({ "result": format!("{r:?}"), "to_list_ms": t })
    } else {
        Value::Null
    };
    let no_shots = o.words.iter().any(|w| w == "no-shots");
    let shot = |name: &str| {
        if no_shots {
            return None;
        }
        let path = format!("{PROBE_DIR}/{name}.png");
        let ok = std::process::Command::new("/usr/sbin/screencapture").args(["-x", &path]).status().is_ok_and(|s| s.success());
        if ok { Some(path) } else { None }
    };
    let before_shot = shot(&format!("transform-{}-before", o.route.name()));
    let initial = sky.get_transform(sid);
    let mut call_ms = Vec::new();
    let mut readback_ok = 0usize;
    let mut listed_ok = 0usize;
    let mut half_shot = None;
    let c0 = cpu_ms();
    let t0 = Instant::now();
    for i in 1..=o.steps {
        let k = 1.0 + (o.scale - 1.0) * i as f64 / o.steps as f64;
        // Scale about the top-left corner, moved right and down by up to 200 points.
        let off = 200.0 * i as f64 / o.steps as f64;
        let t = CGAffineTransform { a: k, b: 0.0, c: 0.0, d: k, tx: off, ty: off };
        let s = Instant::now();
        let _ = sky.set_transform(o.route, sid, t);
        call_ms.push(ms(s));
        if let Some(r) = sky.get_transform(sid)
            && (r[0] - k).abs() < 1e-6
        {
            readback_ok += 1;
        }
        // The window list reports the transformed size: did this step reach the server?
        if let (Some(first), Some(w0)) = (windows::describe(&ids[..1]).first(), list.first())
            && (first.bounds.w - w0.bounds.w / k).abs() < 2.0
        {
            listed_ok += 1;
        }
        if i == o.steps / 2 {
            pump(Duration::from_millis(o.hold_ms));
            half_shot = shot(&format!("transform-{}-half", o.route.name()));
        }
        if o.pace_ms > 0.0 {
            let due = Duration::from_secs_f64(o.pace_ms * i as f64 / 1e3);
            while t0.elapsed() < due {
                pump(Duration::from_micros(200));
            }
        }
    }
    let total = ms(t0);
    let cpu = cpu_ms() - c0;
    pump(Duration::from_millis(o.hold_ms));
    let end_shot = shot(&format!("transform-{}-end", o.route.name()));
    let listed = frames(&windows::describe(&ids));
    let _ = sky.set_transform(o.route, sid, CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 });
    let mut sorted = call_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| sorted.get(((sorted.len() as f64 - 1.0) * p).round() as usize).copied();
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({
        "ok": true, "route": o.route.name(), "space": info, "add": format!("{add:?}"), "shown": shown,
        "initial_transform": initial, "final_readback": sky.get_transform(sid),
        "steps": o.steps, "readback_matched": readback_ok, "listed_matched": listed_ok, "total_ms": total, "cpu_ms": cpu,
        "steps_per_s": (o.steps as f64 / (total / 1e3)).round(), "call_ms_p50": pct(0.5), "call_ms_p95": pct(0.95), "call_ms_max": pct(1.0),
        "listed_while_transformed": listed, "shots": [before_shot, half_shot, end_shot], "teardown": down,
    }))
}

/// S4: matrices set on shown Spaces holding the live windows (see the module docs).
fn affine(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    let word = |name: &str| o.words.iter().position(|w| w == name).and_then(|i| o.words.get(i + 1));
    let per_window = o.words.iter().any(|w| w == "--per-window");
    let no_shots = o.words.iter().any(|w| w == "no-shots");
    let fly: usize = word("--fly").and_then(|v| v.parse().ok()).unwrap_or(0);
    let matrices: Vec<[f64; 6]> = word("--m")
        .map(|text| {
            text.split(';')
                .filter_map(|m| {
                    let v: Vec<f64> = m.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    <[f64; 6]>::try_from(v).ok()
                })
                .collect()
        })
        .unwrap_or_default();
    // `--only I`: just the I-th listed window goes into a Space (the others stay on the Desktop).
    let ids = match word("--only").and_then(|v| v.parse::<usize>().ok()).and_then(|i| ids.get(i).copied()) {
        Some(one) => vec![one],
        None => ids,
    };
    let groups: Vec<Vec<u32>> = if per_window { ids.iter().map(|id| vec![*id]).collect() } else { vec![ids.clone()] };
    let add_route = o.add_route.unwrap_or(o.route);
    // Set up: a Space per group, the windows in, the Space shown.
    let t0 = Instant::now();
    let mut spaces = Vec::new();
    for g in &groups {
        let (sid, _) = new_space(sky, o)?;
        let _ = sky.add_remove(add_route, sid, g);
        let _ = sky.show_hide(o.route, &[sid], true);
        spaces.push(sid);
    }
    let setup_call_ms = ms(t0);
    let (setup_onscreen_ms, _) = wait_onscreen(&ids, true, Duration::from_secs(1));
    let setup_ms = ms(t0);
    let set_all = |m: [f64; 6]| {
        for sid in &spaces {
            let _ = sky.set_transform(o.route, *sid, CGAffineTransform { a: m[0], b: m[1], c: m[2], d: m[3], tx: m[4], ty: m[5] });
        }
    };
    let mut stills = Vec::new();
    for (i, m) in matrices.iter().enumerate() {
        set_all(*m);
        pump(Duration::from_millis(o.hold_ms));
        let path = format!("{PROBE_DIR}/affine-{i}.png");
        let shot = !no_shots && std::process::Command::new("/usr/sbin/screencapture").args(["-x", &path]).status().is_ok_and(|s| s.success());
        stills.push(json!({
            "m": m, "readback": sky.get_transform(spaces[0]), "listed": frames(&windows::describe(&ids)),
            "shot": shot.then_some(path),
        }));
    }
    // The flight: each window's Space shrunk to a fifth toward the left edge, then back.
    let mut step_ms = Vec::new();
    let mut late = 0usize;
    let c0 = cpu_ms();
    let f0 = Instant::now();
    for i in 1..=fly {
        let u = i as f64 / fly as f64;
        let p = if u < 0.5 { u * 2.0 } else { 2.0 - u * 2.0 };
        let s = Instant::now();
        for (n, sid) in spaces.iter().enumerate() {
            // Drawn at 1/k about the window's own corner, moved left: screen = (space - t) / k.
            let k = 1.0 + 4.0 * p;
            let b = list.iter().find(|w| groups[n].contains(&w.id)).map(|w| w.bounds).unwrap_or_default();
            let (to_x, to_y) = (b.x + (40.0 - b.x) * p, b.y + (300.0 + 120.0 * n as f64 - b.y) * p);
            let _ = sky.set_transform(o.route, *sid, CGAffineTransform { a: k, b: 0.0, c: 0.0, d: k, tx: b.x - k * to_x, ty: b.y - k * to_y });
        }
        step_ms.push(ms(s));
        if o.pace_ms > 0.0 {
            let due = Duration::from_secs_f64(o.pace_ms * i as f64 / 1e3);
            if f0.elapsed() > due {
                late += 1;
            }
            while f0.elapsed() < due {
                pump(Duration::from_micros(200));
            }
        }
    }
    let fly_ms = ms(f0);
    let fly_cpu = cpu_ms() - c0;
    // Identity at rest, then everything back.
    set_all([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let hold = word("--rest-ms").and_then(|v| v.parse().ok()).unwrap_or(0u64);
    pump(Duration::from_millis(hold));
    let t1 = Instant::now();
    let (current, _) = sky.current();
    let _ = sky.add_remove_from(add_route, current, &ids, ALL_SPACES_MASK);
    let back_call_ms = ms(t1);
    let (back_ms, _) = wait_onscreen(&ids, true, Duration::from_secs(1));
    let after_spaces: Vec<Vec<u64>> = ids.iter().map(|id| sky.spaces_of(&[*id])).collect();
    let down: Vec<Value> = spaces.iter().zip(&groups).map(|(sid, g)| tear_down(sky, *sid, g, o.route)).collect();
    step_ms.sort_by(f64::total_cmp);
    let pct = |p: f64| step_ms.get(((step_ms.len() as f64 - 1.0) * p).round() as usize).copied();
    Ok(json!({
        "ok": true, "windows": ids.len(), "spaces": spaces.len(), "before": frames(&list),
        "setup_call_ms": setup_call_ms, "setup_ms": setup_ms, "setup_onscreen_ms": setup_onscreen_ms,
        "stills": stills,
        "fly": { "steps": fly, "total_ms": fly_ms, "cpu_ms": fly_cpu, "late_steps": late, "step_ms_p50": pct(0.5), "step_ms_p95": pct(0.95), "step_ms_max": pct(1.0) },
        "back_call_ms": back_call_ms, "back_onscreen_ms": back_ms, "spaces_after": after_spaces,
        "current": current, "teardown": down,
    }))
}

/// Q4: new fixture windows into a hidden Space as soon as they are listed.
fn place_new(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let old: Vec<u32> = windows::list_windows().iter().filter(|w| w.layer == 0).map(|w| w.id).collect();
    let (sid, info) = new_space(sky, o)?;
    print(json!({ "event": "waiting", "space": info, "old": old.len() }));
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(o.hold);
    // Per placed window: id, when first listed, on screen when listed, call ms, when first
    // listed off screen, on-screen samples after the call, samples.
    struct Placed {
        id: u32,
        listed: Instant,
        on_when_listed: bool,
        bounds: crate::engine::geometry::Rect,
        call: String,
        call_ms: f64,
        off_ms: Option<f64>,
        on_after: usize,
        samples: usize,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let mut pids = fixture_pids(bundle);
    let mut last_pids = Instant::now();
    while Instant::now() < until {
        if last_pids.elapsed() > Duration::from_millis(20) {
            pids = fixture_pids(bundle);
            last_pids = Instant::now();
        }
        // One query of every window (on screen or not): a window may be listed before it shows.
        let all = windows::every_window();
        let now = Instant::now();
        for w in &all {
            if !pids.contains(&w.pid) || w.layer != 0 || w.bounds.h <= 50.0 || old.contains(&w.id) {
                continue;
            }
            if let Some(p) = placed.iter_mut().find(|p| p.id == w.id) {
                if now.duration_since(p.listed) < Duration::from_millis(1500) {
                    p.samples += 1;
                    if w.onscreen {
                        p.on_after += 1;
                    } else if p.off_ms.is_none() {
                        p.off_ms = Some((now.duration_since(p.listed).as_secs_f64() * 1e6).round() / 1e3);
                    }
                }
                continue;
            }
            let c = Instant::now();
            let r = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &[w.id]);
            placed.push(Placed {
                id: w.id,
                listed: now,
                on_when_listed: w.onscreen,
                bounds: w.bounds,
                call: format!("{r:?}"),
                call_ms: ms(c),
                off_ms: None,
                on_after: 0,
                samples: 0,
            });
        }
        pump(Duration::from_micros(250));
    }
    let ids: Vec<u32> = placed.iter().map(|p| p.id).collect();
    let events: Vec<Value> = placed
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "t_ms": (p.listed.duration_since(t0).as_secs_f64() * 1e3).round(), "onscreen_when_listed": p.on_when_listed,
                "bounds_when_listed": p.bounds, "call": p.call, "call_ms": p.call_ms, "listed_to_offscreen_ms": p.off_ms,
                "onscreen_samples_after": p.on_after, "samples": p.samples, "spaces": sky.spaces_of(&[p.id]),
            })
        })
        .collect();
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({ "ok": true, "event": "done", "placed": events, "teardown": down }))
}

/// Prints each change of the fixture windows' state for a while.
fn watch(sky: &Sky, bundle: &str, o: &Opts) -> Value {
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(o.hold);
    let mut last = Value::Null;
    let mut changes = 0;
    while Instant::now() < until {
        let list = fixture_windows(sky, &fixture_pids(bundle));
        let state = json!(list.iter().map(|w| json!([w.id, w.onscreen, sky.spaces_of(&[w.id])])).collect::<Vec<_>>());
        if state != last {
            let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            print(json!({ "event": "state", "t_ms": ms(t0), "epoch": epoch, "windows": state }));
            last = state;
            changes += 1;
        }
        pump(Duration::from_millis(o.every_ms));
    }
    json!({ "ok": true, "changes": changes })
}

fn key(o: &Opts) -> Result<Value, String> {
    use super::space_keys::{self, Direction, ESCAPE_KEY, Expose, SpaceKey};
    let word = o.words.first().map(String::as_str).unwrap_or("");
    let r = match word {
        "escape" => space_keys::post_plain(ESCAPE_KEY),
        // ⌘Tab once: to the app used before the front one.
        "cmd-tab" => space_keys::post_with_command(48),
        w => {
            if let Some(d) = Direction::parse(w) {
                space_keys::post(&SpaceKey::read(d), false)
            } else if let Some(e) = Expose::parse(w) {
                space_keys::post(&SpaceKey::read_expose(e), false)
            } else {
                return Err("key takes mission-control, app-windows, escape, cmd-tab, left or right".into());
            }
        }
    };
    r.map(|_| json!({ "ok": true, "key": word, "accessibility": super::permissions::accessibility_trusted() }))
}

fn dock_click(bundle: &str) -> Result<Value, String> {
    let dock = super::dock::dock_pid().ok_or("no Dock")?;
    let (item, info) = super::dock::item_for_bundle(dock, bundle).ok_or("the fixture has no Dock item")?;
    item.perform_action("AXPress").map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "pressed": info.path }))
}

/// `space-op show|hide|destroy SPACE`: one call on a Space the probe recorded (an orphan a
/// killed holder left), timed, with the fixture windows' state after.
fn space_op(sky: &Sky, o: &Opts) -> Result<Value, String> {
    let (Some(op), Some(sid)) = (o.words.first(), o.words.get(1).and_then(|s| s.parse::<u64>().ok())) else {
        return Err("space-op takes show|hide|destroy and a Space id".into());
    };
    if !recorded().contains(&sid) {
        return Err("space-op acts only on a Space the probe recorded".into());
    }
    let ids = ids_of(&fixture_windows(sky, &all_fixture_pids()));
    let t0 = Instant::now();
    let r = match op.as_str() {
        "show" => sky.show_hide(o.route, &[sid], true),
        "hide" => sky.show_hide(o.route, &[sid], false),
        "destroy" => sky.destroy(o.route, sid),
        _ => return Err("space-op takes show|hide|destroy".into()),
    };
    let call = ms(t0);
    let want = op != "hide";
    let (t, now) = wait_onscreen(&ids, want, Duration::from_secs(2));
    pump(Duration::from_millis(150));
    let exists = sky.all_spaces(ALL_SPACES_MASK).contains(&sid);
    if !exists {
        unrecord(sid);
    }
    Ok(json!({
        "ok": true, "op": op, "route": o.route.name(), "space": sid, "result": format!("{r:?}"), "call_ms": call,
        "to_list_ms": t, "exists_after": exists, "frames": frames(&now),
        "spaces": ids.iter().map(|&i| json!([i, sky.spaces_of(&[i])])).collect::<Vec<_>>(),
    }))
}

/// Every fixture window back to the Desktop shown; every recorded Space shown and destroyed.
fn cleanup(sky: &Sky, bundle: Option<&str>) -> Value {
    let pids = match bundle {
        Some(b) => fixture_pids(b),
        None => all_fixture_pids(),
    };
    let list = fixture_windows(sky, &pids);
    let ids = ids_of(&list);
    let (current, _) = sky.current();
    let mut steps = Vec::new();
    let off: Vec<u32> = list.iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
    if !off.is_empty() {
        for r in [Route::Bridged, Route::Classic] {
            let still: Vec<u32> = windows::describe(&off).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
            if still.is_empty() {
                break;
            }
            steps.push(json!({ "route": r.name(), "move": format!("{:?}", sky.move_managed(r, &still, current)) }));
            let _ = wait_onscreen(&still, true, Duration::from_millis(500));
            let still: Vec<u32> = windows::describe(&off).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
            if !still.is_empty() {
                steps.push(json!({ "route": r.name(), "add": format!("{:?}", sky.add_remove(r, current, &still)) }));
                let _ = wait_onscreen(&still, true, Duration::from_millis(500));
            }
        }
    }
    let mut spaces = Vec::new();
    // Never a Desktop, whatever the record says.
    let desktops: std::collections::BTreeSet<u64> = sky.displays().iter().flat_map(|(_, _, spaces)| spaces.iter().map(|(id, _)| *id)).collect();
    for sid in recorded().into_iter().filter(|sid| !desktops.contains(sid)) {
        let mut gone = !sky.all_spaces(0xf).contains(&sid);
        if !gone {
            let _ = sky.show_hide(Route::Bridged, &[sid], true);
            let _ = sky.show_hide(Route::Classic, &[sid], true);
            for r in [Route::Bridged, Route::Classic] {
                let _ = sky.destroy(r, sid);
                pump(Duration::from_millis(150));
                gone = !sky.all_spaces(0xf).contains(&sid);
                if gone {
                    break;
                }
            }
        }
        if gone {
            unrecord(sid);
        }
        spaces.push(json!({ "space": sid, "gone": gone }));
    }
    let after = windows::describe(&ids);
    json!({
        "ok": after.iter().all(|w| w.onscreen),
        "windows": frames(&after),
        "minimized_or_hidden_note": "a fixture window may be off screen for its own reasons (minimized): check frames",
        "steps": steps,
        "spaces": spaces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review A-14: only the Spaces recorded in this window server session are the probe's;
    /// a line of another session, or one with no session, is skipped.
    #[test]
    fn only_this_sessions_spaces_are_the_probes() {
        let now = "boot B session 7 audit 100007";
        let text = format!("12 classic pid=1\tsession={now}\n13 bridged pid=2\tsession=boot A session 7 audit 100007\n14 classic pid=3\n15 classic pid=4\tsession={now}\n");
        assert_eq!(recorded_in(&text, Some(now)), vec![12, 15]);
        assert!(recorded_in(&text, None).is_empty(), "no session known: none is the probe's for sure");
    }
}
