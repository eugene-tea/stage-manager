//! `stage-manager probe-patch [--out FILE.png] [--rect X,Y,W,H] [--reach PT] [--placement
//! fill|fit|stretch|center|tile]`: renders what
//! the wallpaper corner patch would show over the main display's corner right now, through
//! the patch's own code path ([`super::corner_patch::picture_for`] and
//! [`super::corner_patch::crop_for`] on a fresh [`crate::platform::wallpaper::Reader`]), to
//! a PNG, and prints what it drew as one JSON line. For the live check: compare the PNG
//! pixel by pixel with a screen capture of the same rectangle taken by hand.
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
use crate::engine::geometry::Rect;
use crate::engine::park::is_parked_in_at;
use crate::engine::wallpaper::Placement;
use crate::platform::wallpaper::{self, Reader};
use crate::platform::{desktop, desktop_items, windows};

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
