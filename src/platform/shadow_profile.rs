//! `stage-manager probe-shadow --profile [--dump DIR]` and `--compare`: the falloff of macOS's
//! window shadow, alpha against distance past the frame, for a key (active) window and an
//! inactive one, and how closely a snapshot's shadow as the overlay draws it matches it
//! (round ghost-shadow; [`crate::ui::window_shadow`]).
//!
//! **Profile.** One titled window of the probe's own at a time, in the middle of the main
//! display's visible frame: inactive (just above the desktop icons, under every window, the
//! app never active), then key (a titled panel at the normal level made key, the app that was
//! in front given its place back after, [`real_window`]). Each window's image with
//! its framing is taken by its window id alone (`CGWindowListCreateImage`,
//! `kCGWindowListOptionIncludingWindow`), as [`super::shadow_probe`] does: no screen and no
//! other window is captured. Printed per state: the alpha (0-255) at every pixel past the
//! frame along the middle of each side and along the diagonal out of each corner, the 1/255
//! and 2/255 reach per side, and the window's corner radius as drawn. `--dump DIR` also writes
//! each state's alpha plane (`<state>.pgm`, with `<state>.json` saying where the frame is).
//!
//! **Colour.** Per state also the colour across the middle of each side's edge, inside out
//! (`edges`), and the window's own picture without its framing (`bare_edges_outside_in`,
//! `kCGWindowImageBoundsIgnoreFraming`, as a snapshot leaves the shadow out): macOS's white
//! inner rim shows in the first and not in the second. `--appearance` draws the probe's
//! windows dark or light (macOS draws a window's shadow by the window's own appearance, not
//! the system's), `--background GREY` fills them with a grey so a rim over the content shows.
//!
//! **Compare.** For each state, the real window's image as above, and the probe's own
//! borderless overlay-like window drawing a snapshot plane exactly as the motion overlay does
//! ([`crate::ui::window_shadow::ShadowLayers`]: the window's own picture without its framing,
//! the shadow under it and the inner rim over it, for the appearance the probe's windows are
//! drawn in), taken the same way at the same frame. Printed: the largest alpha difference
//! outside the frame, along each profile and over the whole margin, and the largest colour
//! difference across each side's edge and near the frame (inside and out).

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSColor, NSPanel, NSView, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGWindowImageOption;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CATransaction};
use serde_json::{Value, json};

use super::capture_pixels::PixelRect;
use super::shadow_probe::{MARGIN, Variant, make_window, spin, window_image};
use super::{desktop, pixels, windows};
use crate::engine::geometry::Rect;
use crate::ui::shadow_look;
use crate::ui::shadow_math::corner_from_inset;
use crate::ui::window_shadow::{ShadowLayers, WindowShadow};

/// An alpha plane: `w` x `h` bytes, top-left first, and where the window's frame is in it.
pub struct AlphaPlane {
    pub w: usize,
    pub h: usize,
    pub alpha: Vec<u8>,
    /// The premultiplied colour, BGRA per pixel as captured (empty for a model's plane).
    pub bgra: Vec<u8>,
    /// The window's image without its framing (shadow), as a snapshot is taken, BGRA, the
    /// frame's size: `None` for a model's plane.
    pub bare: Option<(Vec<u8>, usize, usize)>,
    /// The same image, for the comparison's snapshot to show.
    pub bare_image: Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>>,
    pub frame: PixelRect,
    /// Pixels per point.
    pub scale: f64,
    /// The window's look as read from its shadow ([`look_of`]); null for a model's plane.
    pub look: Value,
}

impl AlphaPlane {
    /// The alpha at `(x, y)`, 0 outside the plane.
    pub fn at(&self, x: isize, y: isize) -> u8 {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return 0;
        }
        self.alpha[y as usize * self.w + x as usize]
    }

    /// The alpha past the frame along the middle of each side (left, top, right, bottom),
    /// from the first pixel outside the frame outward, `n` pixels each.
    pub fn sides(&self, n: usize) -> [Vec<u8>; 4] {
        let f = &self.frame;
        let (cx, cy) = ((f.x + f.w / 2) as isize, (f.y + f.h / 2) as isize);
        let (x0, y0, x1, y1) = (f.x as isize, f.y as isize, (f.x + f.w) as isize, (f.y + f.h) as isize);
        let take = |step: &dyn Fn(isize) -> (isize, isize)| (0..n as isize).map(|d| { let (x, y) = step(d); self.at(x, y) }).collect();
        [take(&|d| (x0 - 1 - d, cy)), take(&|d| (cx, y0 - 1 - d)), take(&|d| (x1 + d, cy)), take(&|d| (cx, y1 + d))]
    }

    /// The alpha along the diagonal out of each corner of the frame (top-left, top-right,
    /// bottom-right, bottom-left), from the first pixel outside it outward, `n` pixels each.
    pub fn corners(&self, n: usize) -> [Vec<u8>; 4] {
        let f = &self.frame;
        let (x0, y0, x1, y1) = (f.x as isize, f.y as isize, (f.x + f.w) as isize, (f.y + f.h) as isize);
        let take = |step: &dyn Fn(isize) -> (isize, isize)| (0..n as isize).map(|d| { let (x, y) = step(d); self.at(x, y) }).collect();
        [
            take(&|d| (x0 - 1 - d, y0 - 1 - d)),
            take(&|d| (x1 + d, y0 - 1 - d)),
            take(&|d| (x1 + d, y1 + d)),
            take(&|d| (x0 - 1 - d, y1 + d)),
        ]
    }

    /// The colour across the middle of each side's edge (left, top, right, bottom), from `n`
    /// pixels inside the frame to `n` outside, as `[alpha, red, green, blue]` premultiplied.
    pub fn edges(&self, n: usize) -> [Vec<[u8; 4]>; 4] {
        let f = &self.frame;
        let (cx, cy) = ((f.x + f.w / 2) as isize, (f.y + f.h / 2) as isize);
        let (x0, y0, x1, y1) = (f.x as isize, f.y as isize, (f.x + f.w) as isize, (f.y + f.h) as isize);
        let n = n as isize;
        let px = |x: isize, y: isize| -> [u8; 4] {
            if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || self.bgra.is_empty() {
                return [0; 4];
            }
            let i = (y as usize * self.w + x as usize) * 4;
            [self.bgra[i + 3], self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
        };
        let take = |step: &dyn Fn(isize) -> (isize, isize)| (-n..n).map(|d| { let (x, y) = step(d); px(x, y) }).collect();
        [take(&|d| (x0 - 1 - d, cy)), take(&|d| (cx, y0 - 1 - d)), take(&|d| (x1 + d, cy)), take(&|d| (cx, y1 + d))]
    }

    /// The corner radius a snapshot of the window reads from its picture without its framing
    /// ([`crate::ui::window_shadow::corner_of`]), `None` without that picture.
    pub fn picture_corner(&self) -> Option<f64> {
        let (bytes, w, h) = self.bare.as_ref()?;
        let diagonal: Vec<u8> = (0..(*w).min(*h) / 4).filter_map(|d| bytes.get((d * w + d) * 4 + 3).copied()).collect();
        crate::ui::shadow_math::corner_from_diagonal(&diagonal, self.scale)
    }

    /// The window's corner radius as drawn (points), read as a snapshot's is
    /// ([`crate::ui::window_shadow::corner_of`]): along the diagonal into the top-left corner,
    /// how far in its own pixels turn more than half opaque (the shadow under the corner is
    /// less), a continuous corner's arc being [`crate::ui::shadow_math::DIAGONAL_INSET`] radii in.
    pub fn corner_radius(&self) -> f64 {
        let f = &self.frame;
        let inset = (0..f.w.min(f.h) / 2).find(|d| self.at((f.x + d) as isize, (f.y + d) as isize) >= 128).unwrap_or(0);
        corner_from_inset(inset as f64 / self.scale)
    }
}

/// How far (pixels) each profile runs: past every shadow at 2x.
const PROFILE_PX: usize = 200;

/// The reach (points) per side where a profile last reaches `threshold`.
fn reach_pt(profiles: &[Vec<u8>; 4], threshold: u8, scale: f64) -> [f64; 4] {
    profiles.each_ref().map(|p| p.iter().rposition(|a| *a >= threshold).map_or(0.0, |i| (i + 1) as f64 / scale))
}

/// Runs the profile or the comparison (see the module docs).
pub fn run(
    mtm: MainThreadMarker,
    main: &crate::engine::model::Display,
    size: NSSize,
    settle: Duration,
    dump: Option<&Path>,
    compare: bool,
    appearance: Option<bool>,
) -> ExitCode {
    // The probe's own windows in the appearance asked for (the system's is never changed).
    crate::ui::form::set_app_appearance(mtm, appearance);
    let system_dark = objc2_foundation::NSUserDefaults::standardUserDefaults()
        .stringForKey(&objc2_foundation::NSString::from_str("AppleInterfaceStyle"))
        .is_some_and(|s| s.to_string() == "Dark");
    let windows_dark = crate::platform::wallpaper::dark_appearance(mtm);
    let mut out = serde_json::Map::new();
    let mut ok = true;
    for key in [false, true] {
        let state = if key { "active" } else { "inactive" };
        let real = match real_window(mtm, main, size, settle, key) {
            Ok(p) => p,
            Err(why) => {
                ok = false;
                out.insert(state.into(), json!({ "ok": false, "why": why }));
                continue;
            }
        };
        if let Some(dir) = dump
            && let Err(why) = write_dump(dir, state, &real)
        {
            eprintln!("probe-shadow: {why}");
        }
        // The window's own pixels as a snapshot has them (its frame alone).
        if let (Some(dir), Some((bytes, w, h))) = (dump, real.bare.as_ref()) {
            let alpha = bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            let bare = AlphaPlane { w: *w, h: *h, alpha, bgra: Vec::new(), bare: None, bare_image: None, frame: PixelRect { x: 0, y: 0, w: *w, h: *h }, scale: real.scale, look: Value::Null };
            let _ = write_dump(dir, &format!("{state}-bare"), &bare);
        }
        let mut row = profile_json(&real);
        if compare {
            let shadow = WindowShadow::of_window(key, windows_dark);
            // The window's whole frame (its title bar included), at the same top-left.
            let frame = NSSize::new(real.frame.w as f64 / real.scale, real.frame.h as f64 / real.scale);
            // The corner as a snapshot reads it, from the window's own picture.
            let corner = corner_override().or_else(|| real.picture_corner()).unwrap_or_else(|| real.corner_radius());
            match drawn_snapshot(mtm, middle(main, size), frame, settle, &shadow, corner, real.bare_image.as_deref()) {
                Ok(ours) => {
                    if let Some(dir) = dump {
                        let _ = write_dump(dir, &format!("{state}-ours"), &ours);
                    }
                    row["compare"] = compare_json(&real, &ours);
                }
                Err(why) => {
                    ok = false;
                    row["compare"] = json!({ "ok": false, "why": why });
                }
            }
        }
        out.insert(state.into(), row);
    }
    println!("{}", json!({ "ok": ok, "display": main.frame, "system_dark": system_dark, "windows_dark": windows_dark, "states": out }));
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn profile_json(p: &AlphaPlane) -> Value {
    let sides = p.sides(PROFILE_PX);
    let corners = p.corners(PROFILE_PX);
    let trim = |v: &Vec<u8>| -> Vec<u8> { v[..v.iter().rposition(|a| *a > 0).map_or(0, |i| i + 1)].to_vec() };
    let side = |r: [f64; 4]| json!({ "left": r[0], "top": r[1], "right": r[2], "bottom": r[3] });
    json!({
        "ok": true,
        "scale": p.scale,
        "corner_radius": p.corner_radius(),
        "picture_corner": p.picture_corner(),
        "reach_1": side(reach_pt(&sides, 1, p.scale)),
        "reach_2": side(reach_pt(&sides, 2, p.scale)),
        "sides": { "left": trim(&sides[0]), "top": trim(&sides[1]), "right": trim(&sides[2]), "bottom": trim(&sides[3]) },
        "corners": { "top_left": trim(&corners[0]), "top_right": trim(&corners[1]), "bottom_right": trim(&corners[2]), "bottom_left": trim(&corners[3]) },
        "edges": edges_json(p),
        "bare_size": p.bare.as_ref().map(|(_, w, h)| json!([w, h])),
        "bare_edges_outside_in": p.bare.as_ref().map(|b| { let e = bare_edges(b, EDGE_PX); json!({ "left": e[0], "top": e[1], "right": e[2], "bottom": e[3] }) }),
        "look": p.look,
    })
}

/// The colour across each side's edge, inside out ([`AlphaPlane::edges`]): the first entry
/// is [`EDGE_PX`] pixels inside the frame, the one at index [`EDGE_PX`] the first outside.
fn edges_json(p: &AlphaPlane) -> Value {
    let e = p.edges(EDGE_PX);
    json!({ "left": e[0], "top": e[1], "right": e[2], "bottom": e[3] })
}

/// How far (pixels) the edge readout reaches each way.
const EDGE_PX: usize = 8;

/// The largest alpha difference between the real window's shadow and ours, outside the frame.
fn compare_json(real: &AlphaPlane, ours: &AlphaPlane) -> Value {
    let n = PROFILE_PX;
    let worst = |a: &[Vec<u8>; 4], b: &[Vec<u8>; 4]| -> [i32; 4] {
        std::array::from_fn(|i| a[i].iter().zip(&b[i]).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0))
    };
    let sides = worst(&real.sides(n), &ours.sides(n));
    let corners = worst(&real.corners(n), &ours.corners(n));
    // Over the whole margin: every pixel outside the frame, the two planes aligned by frame.
    let f = &real.frame;
    let (dx, dy) = (ours.frame.x as isize - f.x as isize, ours.frame.y as isize - f.y as isize);
    let (mut largest, mut at, mut sum, mut count) = (0i32, (0isize, 0isize), 0u64, 0u64);
    for y in 0..real.h as isize {
        for x in 0..real.w as isize {
            let inside = x >= f.x as isize && x < (f.x + f.w) as isize && y >= f.y as isize && y < (f.y + f.h) as isize;
            if inside {
                continue;
            }
            let d = (real.at(x, y) as i32 - ours.at(x + dx, y + dy) as i32).abs();
            sum += d as u64;
            count += 1;
            if d > largest {
                largest = d;
                at = (x - f.x as isize, y - f.y as isize);
            }
        }
    }
    // The colour (premultiplied, every channel) on screen over the window's edge: outside it
    // and within EDGE_PX pixels inside it (the inner rim over the content), where both planes
    // carry colour.
    let (mut color_out, mut color_edge, mut color_at) = (0i32, 0i32, (0isize, 0isize));
    if !real.bgra.is_empty() && !ours.bgra.is_empty() {
        let e = EDGE_PX as isize;
        let (fx0, fy0, fx1, fy1) = (f.x as isize, f.y as isize, (f.x + f.w) as isize, (f.y + f.h) as isize);
        let px = |p: &AlphaPlane, x: isize, y: isize| -> [i32; 4] {
            if x < 0 || y < 0 || x as usize >= p.w || y as usize >= p.h {
                return [0; 4];
            }
            let i = (y as usize * p.w + x as usize) * 4;
            [p.bgra[i] as i32, p.bgra[i + 1] as i32, p.bgra[i + 2] as i32, p.bgra[i + 3] as i32]
        };
        for y in (fy0 - e).max(0)..(fy1 + e).min(real.h as isize) {
            for x in (fx0 - e).max(0)..(fx1 + e).min(real.w as isize) {
                let inside = x >= fx0 && x < fx1 && y >= fy0 && y < fy1;
                let deep = x >= fx0 + e && x < fx1 - e && y >= fy0 + e && y < fy1 - e;
                if deep {
                    continue;
                }
                let (a, b) = (px(real, x, y), px(ours, x + dx, y + dy));
                let d = (0..4).map(|k| (a[k] - b[k]).abs()).max().unwrap_or(0);
                let slot = if inside { &mut color_edge } else { &mut color_out };
                if d > *slot {
                    *slot = d;
                    if inside {
                        color_at = (x - fx0, y - fy0);
                    }
                }
            }
        }
    }
    // Across the middle of each side, inside out: the inner rim and the hairline where the
    // edge is straight.
    let edge_worst: Vec<i32> = real
        .edges(EDGE_PX)
        .iter()
        .zip(ours.edges(EDGE_PX).iter())
        .map(|(a, b)| a.iter().zip(b).map(|(x, y)| (0..4).map(|k| (x[k] as i32 - y[k] as i32).abs()).max().unwrap_or(0)).max().unwrap_or(0))
        .collect();
    let pt = |px: isize| px as f64 / real.scale;
    json!({
        "ok": true,
        "color_across_sides": { "left": edge_worst[0], "top": edge_worst[1], "right": edge_worst[2], "bottom": edge_worst[3] },
        "edges_ours": edges_json(ours),
        "largest_color_outside_near": color_out,
        "largest_color_edge_inside": color_edge,
        "largest_color_edge_inside_at_pt": [pt(color_at.0), pt(color_at.1)],
        "largest": largest,
        "largest_at_pt_from_frame_top_left": [pt(at.0), pt(at.1)],
        "mean": sum as f64 / count.max(1) as f64,
        "sides": { "left": sides[0], "top": sides[1], "right": sides[2], "bottom": sides[3] },
        "corners": { "top_left": corners[0], "top_right": corners[1], "bottom_right": corners[2], "bottom_left": corners[3] },
        "ours": { "sides": profile_json(ours)["sides"].clone(), "corners": profile_json(ours)["corners"].clone() },
    })
}

/// Writes `plane` as `DIR/<name>.pgm` and its frame as `DIR/<name>.json`.
fn write_dump(dir: &Path, name: &str, plane: &AlphaPlane) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut pgm = format!("P5\n{} {}\n255\n", plane.w, plane.h).into_bytes();
    pgm.extend_from_slice(&plane.alpha);
    std::fs::write(dir.join(format!("{name}.pgm")), pgm).map_err(|e| e.to_string())?;
    let f = &plane.frame;
    let meta = json!({ "frame": { "x": f.x, "y": f.y, "w": f.w, "h": f.h }, "scale": plane.scale });
    std::fs::write(dir.join(format!("{name}.json")), meta.to_string()).map_err(|e| e.to_string())
}

/// The middle of the main display's visible frame for a window of `size` (global, top-left).
fn middle(main: &crate::engine::model::Display, size: NSSize) -> (f64, f64) {
    ((main.visible.x + (main.visible.w - size.width) / 2.0).round(), (main.visible.y + (main.visible.h - size.height) / 2.0).round())
}

/// Places `w` with its top-left at global `(x, y)`.
fn place(mtm: MainThreadMarker, w: &NSWindow, (x, y): (f64, f64)) {
    let primary = desktop::primary_height(mtm);
    let frame = w.frame();
    w.setFrame_display(NSRect::new(NSPoint::new(x, primary - y - frame.size.height), frame.size), false);
}

/// The alpha plane of window `id` with its framing, over its listed frame grown by
/// [`MARGIN`], and that frame.
fn plane_of(id: u32) -> Result<AlphaPlane, String> {
    let listed = windows::describe(&[id]).into_iter().next().map(|w| w.bounds).ok_or("the window server does not list the window")?;
    let grown = Rect::new(listed.x - MARGIN, listed.y - MARGIN, listed.w + 2.0 * MARGIN, listed.h + 2.0 * MARGIN);
    let image = window_image(id, Some(grown)).ok_or("no image of the window (CGWindowListCreateImage gave none)")?;
    let (bytes, w, h) = pixels::bgra(&image, None).ok_or("the image could not be read")?;
    let scale = w as f64 / grown.w;
    let frame = PixelRect {
        x: (MARGIN * scale).round() as usize,
        y: (MARGIN * scale).round() as usize,
        w: (listed.w * scale).round() as usize,
        h: (listed.h * scale).round() as usize,
    };
    let alpha = bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    Ok(AlphaPlane { w, h, alpha, bgra: bytes, bare: None, bare_image: None, frame, scale, look: Value::Null })
}

/// The probe windows' background grey (0-1, sRGB) when `--background` set one: a rim over a
/// window's content shows against it.
static BACKGROUND: std::sync::Mutex<Option<f64>> = std::sync::Mutex::new(None);

/// Sets the probe windows' background grey (`--background`).
pub fn set_background(grey: Option<f64>) {
    *BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = grey;
}

/// The corner radius the comparison's shadow is drawn at when `--corner` set one (else the
/// one read from the window's picture, as a snapshot's is).
static CORNER: std::sync::Mutex<Option<f64>> = std::sync::Mutex::new(None);

/// Sets the comparison's corner radius (`--corner`).
pub fn set_corner(r: Option<f64>) {
    *CORNER.lock().unwrap_or_else(|e| e.into_inner()) = r;
}

fn corner_override() -> Option<f64> {
    *CORNER.lock().unwrap_or_else(|e| e.into_inner())
}

fn background() -> Option<f64> {
    *BACKGROUND.lock().unwrap_or_else(|e| e.into_inner())
}

/// Window `id`'s image without its framing (`kCGWindowImageBoundsIgnoreFraming`): its own
/// pixels, as a snapshot (shadows ignored) has them.
fn bare_image_of(id: u32) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
    let opt = CGWindowImageOption::BestResolution | CGWindowImageOption::BoundsIgnoreFraming;
    let rect = CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::new(0.0, 0.0));
    // Our own window only (kCGWindowListOptionIncludingWindow), as [`window_image`].
    #[allow(deprecated)]
    objc2_core_graphics::CGWindowListCreateImage(rect, objc2_core_graphics::CGWindowListOption::OptionIncludingWindow, id, opt)
}

/// The colour across the middle of each side of a bare image (`w` x `h`, the frame), from
/// the outermost pixel inward, `n` each, `[alpha, red, green, blue]` premultiplied.
fn bare_edges((bytes, w, h): &(Vec<u8>, usize, usize), n: usize) -> [Vec<[u8; 4]>; 4] {
    let px = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * w + x) * 4;
        bytes.get(i..i + 4).map_or([0; 4], |b| [b[3], b[2], b[1], b[0]])
    };
    let (cx, cy) = (w / 2, h / 2);
    let n = n.min(w / 2).min(h / 2);
    [
        (0..n).map(|d| px(d, cy)).collect(),
        (0..n).map(|d| px(cx, d)).collect(),
        (0..n).map(|d| px(w - 1 - d, cy)).collect(),
        (0..n).map(|d| px(cx, h - 1 - d)).collect(),
    ]
}

/// A titled window of the probe's own in the middle: inactive under every window, or key
/// (`key`). macOS refuses `activate()` for the probe's app while another app is in front
/// (cooperative activation, macOS 14 on), so the key window is a titled panel
/// (`NSWindowStyleMaskNonactivatingPanel`) made key at the normal level, which macOS lets
/// become key and draws with the key window's shadow (on the owner's Mac, 11-01, it also
/// made the probe's app active); the app that was in front is activated again after.
fn real_window(mtm: MainThreadMarker, main: &crate::engine::model::Display, size: NSSize, settle: Duration, key: bool) -> Result<AlphaPlane, String> {
    let app = NSApplication::sharedApplication(mtm);
    let window: Retained<NSWindow> = if key {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(NSPanel::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), size), style, NSBackingStoreType::Buffered, false);
        // SAFETY: the panel is owned here until closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&objc2_foundation::NSString::from_str("Shadow probe"));
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(false);
        panel.setLevel(objc2_app_kit::NSNormalWindowLevel);
        Retained::into_super(panel)
    } else {
        make_window(mtm, Variant { toolbar: false, corner: false, normal_level: false }, size)
    };
    if let Some(grey) = background() {
        window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(grey, grey, grey, 1.0)));
    }
    let at = middle(main, size);
    // The app in front, given its place back after (the panel may activate the probe's app).
    let before = key.then(|| objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()).flatten();
    let result = (|| {
        place(mtm, &window, at);
        if key {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderFrontRegardless();
        }
        place(mtm, &window, at);
        spin(settle);
        if key != window.isKeyWindow() || (!key && app.isActive()) {
            return Err(format!(
                "{} (key window: {}, app active: {})",
                if key { "the probe's panel did not become key" } else { "the probe's window became active" },
                window.isKeyWindow(),
                app.isActive()
            ));
        }
        let id = u32::try_from(window.windowNumber()).map_err(|_| "no window number".to_string())?;
        let mut plane = plane_of(id)?;
        plane.look = look_of(id, &plane);
        if let Some(image) = bare_image_of(id) {
            plane.bare = pixels::bgra(&image, None);
            plane.bare_image = Some(image);
        }
        if plane.at((plane.frame.x + plane.frame.w / 2) as isize, (plane.frame.y + plane.frame.h / 2) as isize) < 200 {
            return Err("the window was not drawn".into());
        }
        Ok(plane)
    })();
    window.orderOut(None);
    window.close();
    if let Some(before) = before
        && app.isActive()
    {
        before.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty());
    }
    spin(Duration::from_millis(150));
    result
}

/// How the captures would read window `id`'s look ([`crate::ui::shadow_look`]): from `plane`
/// (its image with its framing at the display's resolution), and, with Screen Recording
/// granted to the process that runs the probe, from the still the captures take
/// ([`crate::platform::capture::look_shot`]: ScreenCaptureKit, half a pixel a point).
fn look_of(id: u32, plane: &AlphaPlane) -> Value {
    let frame = (plane.frame.w as f64 / plane.scale, plane.frame.h as f64 / plane.scale);
    let display = look_json(shadow_look::profiles(&plane.alpha, plane.w, plane.h, frame), (plane.w, plane.h));
    let captures = if objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        *LOOK_SHOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::platform::capture::look_shot(id, look_shot_sink);
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(v) = LOOK_SHOT.lock().unwrap_or_else(|e| e.into_inner()).take() {
                break v;
            }
            if std::time::Instant::now() >= until {
                break json!({ "ok": false, "why": "no still of the window came back" });
            }
            spin(Duration::from_millis(20));
        }
    } else {
        json!({ "ok": false, "why": "Screen Recording is not granted to the process running the probe" })
    };
    json!({ "display": display, "captures": captures })
}

/// The reading of [`crate::platform::capture::look_shot`]'s still, as JSON.
static LOOK_SHOT: std::sync::Mutex<Option<Value>> = std::sync::Mutex::new(None);

fn look_shot_sink(_id: u32, _generation: u64, image: &objc2_core_graphics::CGImage, frame: (f64, f64)) {
    let v = match pixels::bgra(image, None) {
        Some((bytes, w, h)) => {
            let alpha: Vec<u8> = bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            look_json(shadow_look::profiles(&alpha, w, h, frame), (w, h))
        }
        None => json!({ "ok": false, "why": "the still could not be read" }),
    };
    *LOOK_SHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
}

/// A reading as JSON: the look, the fit's error and the other appearance's, the scale and
/// the first pixels of each profile.
fn look_json(profiles: Option<Vec<shadow_look::Profile>>, size: (usize, usize)) -> Value {
    let Some(profiles) = profiles else { return json!({ "ok": false, "size": size, "why": "the window was not found in its picture" }) };
    let first: Vec<Value> = profiles.iter().map(|p| json!({ "side": format!("{:?}", p.side), "alpha": p.alpha.iter().take(16).collect::<Vec<_>>() })).collect();
    let k = profiles.first().map_or(0.0, |p| p.px_per_pt);
    match shadow_look::classify(&profiles) {
        Some(f) => {
            let (look, state) = f.look.words();
            json!({ "ok": true, "look": look, "state": state, "rms": (f.rms * 10.0).round() / 10.0, "other": (f.other * 10.0).round() / 10.0, "px_per_pt": (k * 1000.0).round() / 1000.0, "size": size, "profiles": first })
        }
        None => json!({ "ok": false, "why": "no clear evidence", "px_per_pt": k, "size": size, "profiles": first }),
    }
}

/// The probe's own borderless window drawing an opaque plane of `size` and `corner` radius
/// with its top-left at global `(x, y)` (the real window's frame), with `shadow` as the motion
/// overlay draws a snapshot's: taken by its id, as the real window is.
fn drawn_snapshot(
    mtm: MainThreadMarker,
    (x, y): (f64, f64),
    size: NSSize,
    settle: Duration,
    shadow: &WindowShadow,
    corner: f64,
    picture: Option<&objc2_core_graphics::CGImage>,
) -> Result<AlphaPlane, String> {
    // The panel reaches past the frame by the margin, the plane at the frame within it.
    let grown = NSSize::new(size.width + 2.0 * MARGIN, size.height + 2.0 * MARGIN);
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), grown),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    // SAFETY: the panel is owned here until closed.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setLevel((objc2_core_graphics::CGWindowLevelForKey(objc2_core_graphics::CGWindowLevelKey::DesktopIconWindowLevelKey) + 1) as isize);
    let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), grown));
    let root = CALayer::new();
    view.setLayer(Some(&root));
    view.setWantsLayer(true);
    panel.setContentView(Some(&view));
    let scale = panel.backingScaleFactor();
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    root.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(grown.width, grown.height)));
    let plane = CALayer::new();
    plane.setAnchorPoint(CGPoint::new(0.0, 0.0));
    plane.setPosition(CGPoint::new(MARGIN, MARGIN));
    plane.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height)));
    let mut layers = ShadowLayers::new(&plane);
    let window = CGSize::new(size.width, size.height);
    layers.set_shadow(shadow, corner, scale, window, window);
    layers.set_amount(1.0);
    let img = CALayer::new();
    img.setAnchorPoint(CGPoint::new(0.0, 0.0));
    img.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height)));
    img.setPosition(CGPoint::new(0.0, 0.0));
    match picture {
        // The real window's own pixels (its corners clear), as a snapshot shows them.
        Some(picture) => {
            // SAFETY: framework constant; a CGImage is valid layer contents.
            unsafe {
                img.setContentsGravity(objc2_quartz_core::kCAGravityResize);
                img.setContents(Some(&*(picture as *const objc2_core_graphics::CGImage).cast::<objc2::runtime::AnyObject>()));
            }
        }
        None => {
            img.setBackgroundColor(Some(&objc2_core_graphics::CGColor::new_srgb(0.95, 0.95, 0.95, 1.0)));
            img.setCornerRadius(corner);
            // SAFETY: framework constant.
            img.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
            img.setMasksToBounds(true);
        }
    }
    plane.addSublayer(&img);
    layers.attach_rim(&plane);
    // Top-down, as the overlay's layers are (a view's own layer cannot keep the flag).
    let flipped = CALayer::new();
    flipped.setGeometryFlipped(true);
    flipped.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(grown.width, grown.height)));
    flipped.addSublayer(&plane);
    root.addSublayer(&flipped);
    CATransaction::commit();
    let primary = desktop::primary_height(mtm);
    panel.setFrame_display(NSRect::new(NSPoint::new(x - MARGIN, primary - (y - MARGIN) - grown.height), grown), false);
    panel.orderFrontRegardless();
    spin(settle);
    let result = (|| {
        let id = u32::try_from(panel.windowNumber()).map_err(|_| "no window number".to_string())?;
        let listed = windows::describe(&[id]).into_iter().next().map(|w| w.bounds).ok_or("the window server does not list the panel")?;
        let frame = Rect::new(listed.x + MARGIN, listed.y + MARGIN, size.width, size.height);
        let shot = Rect::new(frame.x - MARGIN, frame.y - MARGIN, frame.w + 2.0 * MARGIN, frame.h + 2.0 * MARGIN);
        let image = window_image(id, Some(shot)).ok_or("no image of the panel")?;
        let (bytes, w, h) = pixels::bgra(&image, None).ok_or("the image could not be read")?;
        let s = w as f64 / shot.w;
        let frame = PixelRect { x: (MARGIN * s).round() as usize, y: (MARGIN * s).round() as usize, w: (size.width * s).round() as usize, h: (size.height * s).round() as usize };
        Ok(AlphaPlane { w, h, alpha: bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect(), bgra: bytes, bare: None, bare_image: None, frame, scale: s, look: Value::Null })
    })();
    panel.orderOut(None);
    panel.close();
    let _keep: Retained<NSView> = view;
    spin(Duration::from_millis(100));
    result
}
