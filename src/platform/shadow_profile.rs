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
//! **Compare.** For each state, the real window's image as above, and the probe's own
//! borderless overlay-like window drawing a snapshot plane exactly as the motion overlay does
//! ([`crate::ui::window_shadow::ShadowLayers`], over an opaque plane of the window's size and
//! corner radius), taken the same way at the same frame. Printed: the largest alpha difference
//! outside the frame, along each profile and over the whole margin.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSColor, NSPanel, NSView, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CATransaction};
use serde_json::{Value, json};

use super::capture_pixels::PixelRect;
use super::shadow_probe::{MARGIN, Variant, make_window, spin, window_image};
use super::{desktop, pixels, windows};
use crate::engine::geometry::Rect;
use crate::ui::shadow_math::corner_from_inset;
use crate::ui::window_shadow::{ShadowLayers, WindowShadow};

/// An alpha plane: `w` x `h` bytes, top-left first, and where the window's frame is in it.
pub struct AlphaPlane {
    pub w: usize,
    pub h: usize,
    pub alpha: Vec<u8>,
    pub frame: PixelRect,
    /// Pixels per point.
    pub scale: f64,
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
pub fn run(mtm: MainThreadMarker, main: &crate::engine::model::Display, size: NSSize, settle: Duration, dump: Option<&Path>, compare: bool) -> ExitCode {
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
        let mut row = profile_json(&real);
        if compare {
            let shadow = if key { WindowShadow::ACTIVE } else { WindowShadow::INACTIVE };
            // The window's whole frame (its title bar included), at the same top-left.
            let frame = NSSize::new(real.frame.w as f64 / real.scale, real.frame.h as f64 / real.scale);
            match drawn_snapshot(mtm, middle(main, size), frame, settle, &shadow, real.corner_radius()) {
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
    println!("{}", json!({ "ok": ok, "display": main.frame, "states": out }));
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
        "reach_1": side(reach_pt(&sides, 1, p.scale)),
        "reach_2": side(reach_pt(&sides, 2, p.scale)),
        "sides": { "left": trim(&sides[0]), "top": trim(&sides[1]), "right": trim(&sides[2]), "bottom": trim(&sides[3]) },
        "corners": { "top_left": trim(&corners[0]), "top_right": trim(&corners[1]), "bottom_right": trim(&corners[2]), "bottom_left": trim(&corners[3]) },
    })
}

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
    let pt = |px: isize| px as f64 / real.scale;
    json!({
        "ok": true,
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
    Ok(AlphaPlane { w, h, alpha, frame, scale })
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
        let plane = plane_of(id)?;
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
    layers.set_shadow(shadow, corner, scale, CGSize::new(size.width, size.height));
    layers.set_amount(1.0);
    let img = CALayer::new();
    img.setAnchorPoint(CGPoint::new(0.0, 0.0));
    img.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height)));
    img.setPosition(CGPoint::new(0.0, 0.0));
    img.setBackgroundColor(Some(&objc2_core_graphics::CGColor::new_srgb(0.95, 0.95, 0.95, 1.0)));
    img.setCornerRadius(corner);
    // SAFETY: framework constant.
    img.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
    img.setMasksToBounds(true);
    plane.addSublayer(&img);
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
        Ok(AlphaPlane { w, h, alpha: bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect(), frame, scale: s })
    })();
    panel.orderOut(None);
    panel.close();
    let _keep: Retained<NSView> = view;
    spin(Duration::from_millis(100));
    result
}
