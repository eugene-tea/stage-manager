//! `stage-manager probe-patch [--out FILE.png] [--rect X,Y,W,H] [--reach PT] [--placement
//! fill|fit|stretch|center|tile]`: renders what
//! the wallpaper corner patch would show over the main display's corner right now, through
//! the patch's own code path ([`super::corner_patch::picture_for`] and
//! [`super::corner_patch::crop_for`] on a fresh [`crate::platform::wallpaper::Reader`]), to
//! a PNG, and prints what it drew as one JSON line, with which way its frame settled
//! (`settled`: `exact`; near a solar frame change `alike` when every candidate frame renders
//! the block alike, else `off`: only the running app settles that, with one still of the
//! desktop, [`super::corner_settle`]). A solar frame is decided by the clock's unnarrowed
//! model (`hypotheses`: how many stand; the probe takes no still, so it never narrows): the
//! running app's model, narrowed by its stills, may name fewer candidates. For the live
//! check: compare the PNG pixel by pixel with a screen capture of the same rectangle taken by
//! hand. `app` is the patch as the running app runs it (`ctl state`'s `corner_patch`: `mode`
//! `ordered`, `fallback` or `unknown`), or why not known.
//!
//! The rectangle is `--rect` (global points, top-left), else the parked windows' slivers
//! on screen now grown by the reach ([`crate::engine::corner_patch::patch_rect`]), else a
//! default corner block. `--placement` draws the same picture as if that placement were
//! picked (a what-if for checking the drawing; the patch never uses it). The PNG is in the display's color space at its backing scale;
//! it defaults to `/tmp/stage-manager-patch.png` and is never written inside a git work
//! tree. Nothing is captured, moved or shown; no app needs to run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;
use serde_json::json;

use super::corner_patch::{crop_for, picture_for};
use crate::engine::corner_patch::{DEFAULT_REACH, DesktopIcons, item_reach, patch_rect};
use crate::engine::geometry::{Rect, Size};
use crate::engine::solar_settle::{self, Thumb};
use crate::engine::park::is_parked_in_at;
use crate::engine::wallpaper::Placement;
use crate::platform::wallpaper::{self, Job, Reader};
use crate::platform::{desktop, desktop_items, windows};

/// The corner patch as the running app runs it (`ctl state`'s `corner_patch`: its `mode`,
/// `ordered` or `fallback`, the Fable review's L3), or why not known: no app runs at this
/// home, or it does not answer.
fn running_app_patch() -> serde_json::Value {
    match crate::platform::ctl::request("state", std::time::Duration::from_secs(2)) {
        Ok(reply) => match serde_json::from_str::<serde_json::Value>(&reply) {
            Ok(v) if v.get("ok") == Some(&json!(true)) => v.get("corner_patch").cloned().unwrap_or(json!({ "mode": "unknown" })),
            Ok(v) => json!({ "mode": "unknown", "why": v.get("error").cloned().unwrap_or(json!("no state")) }),
            Err(e) => json!({ "mode": "unknown", "why": e.to_string() }),
        },
        Err(e) => json!({ "mode": "unknown", "why": format!("no running app answers: {e}") }),
    }
}

/// Where the PNG goes by default.
pub const DEFAULT_OUT: &str = "/tmp/stage-manager-patch.png";

/// The default corner block when no sliver shows (points).
const FALLBACK: (f64, f64) = (120.0, 200.0);

fn usage() -> ExitCode {
    eprintln!("usage: stage-manager probe-patch [--out FILE.png] [--rect X,Y,W,H] [--reach PT] [--placement fill|fit|stretch|center|tile]");
    ExitCode::from(2)
}

/// True when `path`'s folder lies inside a git work tree.
fn in_git_tree(path: &Path) -> bool {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let Ok(dir) = dir.canonicalize() else { return false };
    dir.ancestors().any(|a| a.join(".git").exists())
}

/// Runs the probe.
pub fn run(args: &[String]) -> ExitCode {
    let (mut out, mut given, mut reach, mut what_if) = (PathBuf::from(DEFAULT_OUT), None, DEFAULT_REACH, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("--out", Some(p)) => out = PathBuf::from(p),
            ("--rect", Some(r)) => {
                let v: Vec<f64> = r.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let [x, y, w, h] = v[..] else { return usage() };
                given = Some(Rect::new(x, y, w, h));
            }
            ("--placement", Some(p)) => match Placement::from_store(if p == "fill" { "crop" } else { p }) {
                Some(p) => what_if = Some(p),
                None => return usage(),
            },
            ("--reach", Some(r)) => match r.parse::<f64>() {
                Ok(v) if v.is_finite() => reach = v,
                _ => return usage(),
            },
            _ => return usage(),
        }
    }
    if in_git_tree(&out) {
        eprintln!("probe-patch: {} is inside a git work tree; write it under /tmp", out.display());
        return ExitCode::from(2);
    }
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
    let _app = NSApplication::sharedApplication(mtm);
    let Some(main) = desktop::main_display(mtm) else {
        println!("{}", json!({ "ok": false, "why": "no main display" }));
        return ExitCode::FAILURE;
    };
    let slivers: Vec<Rect> = windows::on_screen()
        .into_iter()
        .filter(|w| w.layer == 0 && is_parked_in_at(w.bounds, main.frame, main.visible, main.corner))
        .map(|w| w.bounds)
        .collect();
    let (rect, source) = match given {
        Some(r) => (r, "given"),
        None => match patch_rect(&slivers, main.frame, reach) {
            Some(r) => (r, "slivers"),
            None => {
                let f = main.frame;
                (Rect::new(f.max_x() - FALLBACK.0, f.max_y() - FALLBACK.1, FALLBACK.0, FALLBACK.1), "default corner")
            }
        },
    };
    let icons_started = Instant::now();
    let read = desktop_items::finder_icons();
    let icons_ms = icons_started.elapsed().as_secs_f64() * 1000.0;
    let (icons, icons_why) = match read {
        Some(DesktopIcons::Icons(i)) => (Some(i), None),
        Some(DesktopIcons::TooMany(n)) => (None, Some(format!("{n} items, more than read"))),
        Some(DesktopIcons::Unexpected(why)) => (None, Some(why.to_string())),
        None => (None, Some("Finder did not answer".to_string())),
    };
    let icons_under = icons.as_ref().map(|i| i.iter().filter(|r| item_reach(**r).intersect(&rect).is_some()).count());
    let read_started = Instant::now();
    let wall = Reader::default().read(mtm);
    let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
    let base = json!({
        "rect": rect,
        "rect_from": source,
        "app": running_app_patch(),
        "slivers": slivers.len(),
        "picture": wall.path.as_deref().map(wallpaper::file_name),
        "dark": wall.dark,
        "icons": icons.as_ref().map(Vec::len),
        "icons_under": icons_under,
        "icons_unknown": icons_why,
        "icons_ms": (icons_ms * 10.0).round() / 10.0,
        "read_ms": (read_ms * 10.0).round() / 10.0,
    });
    let (mut decision, mut job) = match &wall.outcome {
        Ok(ok) => ok.clone(),
        Err(why) => {
            println!("{}", json!({ "ok": false, "why": why, "probe": base }));
            return ExitCode::FAILURE;
        }
    };
    if let Some(p) = what_if {
        decision.placement = p;
        job.placement = p;
    }
    let started = Instant::now();
    let hypotheses = decision.sun.as_ref().map(|s| s.surviving);
    let mut settled = json!({ "way": "exact", "hypotheses": hypotheses });
    if !decision.candidates.is_empty() {
        // Near a solar frame change: the candidates' renders of the block, compared as the
        // patch compares them.
        let region = crate::engine::wallpaper::region(Size::new(wall.screen.w, wall.screen.h), crate::engine::wallpaper::local(rect, wall.screen));
        let size = Size::new(region.w, region.h);
        let thumbs: Vec<Option<Thumb>> = decision
            .candidates
            .iter()
            .map(|i| wallpaper::render(&Job { index: *i, ..job.clone() }, region).ok().and_then(|img| wallpaper::thumb(&img, job.space.as_deref(), size)))
            .collect();
        let read: Vec<&Thumb> = thumbs.iter().flatten().collect();
        let alike = read.len() == thumbs.len() && solar_settle::alike(&read);
        let spread = read.windows(2).filter_map(|w| solar_settle::max_diff(w[0], w[1])).fold(0.0, f64::max);
        settled = json!({
            "way": if alike { "alike" } else { "off" },
            "candidates": decision.candidates,
            "hypotheses": hypotheses,
            "max_cell_diff": (spread * 10.0).round() / 10.0,
            "alike_levels": solar_settle::ALIKE_LEVELS,
        });
        if !alike {
            let why = "the candidate frames differ here; the running app settles that with one still of the desktop";
            println!("{}", json!({ "ok": false, "why": why, "settled": settled, "probe": base }));
            return ExitCode::FAILURE;
        }
    }
    let drawn = picture_for(&job, wall.screen, rect).and_then(|(region, picture)| {
        crop_for(&picture, region, job.scale, wall.screen, rect).map(|c| (region, c)).ok_or_else(|| "the crop is outside the render".to_string())
    });
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (region, image) = match drawn {
        Ok(d) => d,
        Err(why) => {
            println!("{}", json!({ "ok": false, "why": why, "probe": base }));
            return ExitCode::FAILURE;
        }
    };
    if let Err(why) = wallpaper::write_png(&image, &out) {
        println!("{}", json!({ "ok": false, "why": why, "probe": base }));
        return ExitCode::FAILURE;
    }
    let px = (objc2_core_graphics::CGImage::width(Some(&image)), objc2_core_graphics::CGImage::height(Some(&image)));
    println!(
        "{}",
        json!({
            "ok": true,
            "out": out,
            "px": [px.0, px.1],
            "scale": job.scale,
            "kind": decision.kind,
            "frame": decision.frame.index,
            "settled": settled,
            "style": decision.style.map(|s| format!("{s:?}").to_lowercase()),
            "placement": decision.placement.name(),
            "color_space": job.space_key,
            "screen": wall.screen,
            "region": region,
            "render_ms": (render_ms * 10.0).round() / 10.0,
            "probe": base,
        })
    );
    ExitCode::SUCCESS
}
