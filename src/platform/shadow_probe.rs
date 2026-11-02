//! `stage-manager probe-shadow [--settle MS] [--size W,H] [--only corner|middle]`: measures
//! how far an inactive window's shadow reaches past its frame on each side, from a window of
//! its own (round fixes-151; the corner patch's per-side reach,
//! [`crate::engine::corner_patch::DEFAULT_REACH`]).
//!
//! The probe opens one window of its own at a time (an accessory app that never activates,
//! the window ordered in without becoming key: inactive, as a parked window is), lets the
//! window server draw it, and takes that one window's image with its framing
//! (`CGWindowListCreateImage`, `kCGWindowListOptionIncludingWindow`, over the window's frame
//! grown by [`MARGIN`] on every side, so the frame's place in the image is known). The alpha
//! that reaches past the frame is the shadow: per side, how far the pixels at or above each
//! of [`THRESHOLDS`] (out of 255) reach ([`reach_px`]). No other window and no screen is
//! captured; the window is closed before the next one opens.
//!
//! Variants, each one window: a plain titled window and one with a unified toolbar (macOS
//! rounds its corners more); in the park corner, as a real parked window (only its top-left
//! point shows, at a level just above the desktop icons, under every normal window), and in
//! the middle of the visible frame at that level (under every window: only a bare desktop
//! there shows it). The titled one is also measured in the corner at the normal level, to
//! tell whether the level changes the shadow. `drawn` says whether the window's own pixels
//! were in the image (an off-screen window the window server did not draw reads as clear).
//!
//! The probe's app is an accessory: no copy of Stage Manager manages its windows (they
//! observe regular apps only), and nothing of the user's is moved or captured.
//!
//! Prints one JSON object: every variant, and `suggested`, the reach per side at 1/255 (the
//! shadow's whole extent, [`SUGGESTED_AT`]) over the variants that were drawn, rounded up to
//! whole points. `--stack N` takes 1, 2, 4 … N windows at one frame together instead, as
//! parked windows share the corner ([`run_stack`]). `--profile [--dump DIR]` measures the
//! shadow's falloff for a key window and an inactive one, and `--compare` also how closely a
//! snapshot's shadow as the overlay draws it matches each ([`super::shadow_profile`]);
//! `--appearance dark|light|system` draws the probe's windows in that appearance (the
//! system's is never changed), `--background GREY` fills them with a grey, `--corner PT`
//! draws the comparison's shadow at that corner radius.

use std::process::ExitCode;
use std::time::Duration;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSToolbar, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGWindowImageOption, CGWindowLevelForKey, CGWindowLevelKey, CGWindowListOption};
use objc2_foundation::{MainThreadMarker, NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};
use serde_json::{Value, json};

use super::capture_pixels::PixelRect;
use super::{desktop, pixels, windows};
use crate::engine::geometry::Rect;

/// How far past the frame (points, every side) the image is taken: more than any shadow.
pub const MARGIN: f64 = 160.0;

/// The alpha levels (out of 255) whose reach is reported.
pub const THRESHOLDS: [u8; 5] = [1, 2, 4, 8, 16];

/// The threshold `suggested` is read at: the shadow's whole extent. One window's tail under
/// 2/255 is invisible, but parked windows share the corner and their tails add up (`--stack`).
pub const SUGGESTED_AT: u8 = 1;

/// How much of the parked window shows in the corner (points down from its top), as on the
/// owner's Mac (Safari keeps 52).
const CORNER_SHOWS: f64 = 52.0;

/// How far a threshold's pixels reach past the frame per side (left, top, right, bottom, in
/// pixels), and whether they touch the image's edge ([`reach_px`]).
pub type Reached = ([usize; 4], bool);

/// How far pixels at or above `threshold` reach past `frame` (pixels within a `w` x `h` image
/// of 4-byte pixels, rows of `row` bytes, alpha at byte `alpha`), per side: left, top, right,
/// bottom, in pixels (0 where nothing reaches past that side), and whether they touch the
/// image's edge (the margin was too small to see where they end). `None` when no pixel is at
/// or above it.
pub fn reach_px(bytes: &[u8], row: usize, w: usize, h: usize, alpha: usize, frame: PixelRect, threshold: u8) -> Option<Reached> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if bytes.get(y * row + x * 4 + alpha).is_some_and(|a| *a >= threshold) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let reach = [frame.x.saturating_sub(x0), frame.y.saturating_sub(y0), x1.saturating_sub(frame.x + frame.w), y1.saturating_sub(frame.y + frame.h)];
    Some((reach, x0 == 0 || y0 == 0 || x1 == w || y1 == h))
}

/// The mean alpha (0-255) of the middle tenth of `frame`: the window's own pixels, opaque
/// when the window server drew it.
pub fn middle_alpha(bytes: &[u8], row: usize, alpha: usize, frame: PixelRect) -> u8 {
    let (bw, bh) = ((frame.w / 10).max(1), (frame.h / 10).max(1));
    let (cx, cy) = (frame.x + frame.w / 2 - bw / 2, frame.y + frame.h / 2 - bh / 2);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in cy..cy + bh {
        for x in cx..cx + bw {
            if let Some(a) = bytes.get(y * row + x * 4 + alpha) {
                sum += u64::from(*a);
                n += 1;
            }
        }
    }
    sum.checked_div(n).unwrap_or(0) as u8
}

/// One window to measure.
#[derive(Debug, Clone, Copy)]
pub(super) struct Variant {
    pub(super) toolbar: bool,
    pub(super) corner: bool,
    pub(super) normal_level: bool,
}

impl Variant {
    fn name(self) -> String {
        format!(
            "{} in the {}, {} level",
            if self.toolbar { "toolbar window" } else { "titled window" },
            if self.corner { "park corner" } else { "middle" },
            if self.normal_level { "normal" } else { "low" }
        )
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: stage-manager probe-shadow [--settle MS] [--size W,H] [--only corner|middle] [--stack N] [--profile [--dump DIR]] [--compare] [--appearance dark|light|system] [--background GREY] [--corner PT]");
    ExitCode::from(2)
}

/// Runs the probe (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let (mut settle, mut size, mut only, mut stack) = (Duration::from_millis(600), NSSize::new(800.0, 500.0), None::<bool>, None::<usize>);
    let (mut profile, mut compare, mut dump, mut appearance) = (false, false, None::<std::path::PathBuf>, None::<bool>);
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                profile = true;
                continue;
            }
            "--compare" => {
                compare = true;
                continue;
            }
            _ => {}
        }
        match (a.as_str(), it.next().map(String::as_str)) {
            ("--dump", Some(dir)) => dump = Some(dir.into()),
            ("--appearance", Some("dark")) => appearance = Some(true),
            ("--appearance", Some("light")) => appearance = Some(false),
            ("--appearance", Some("system")) => appearance = None,
            ("--corner", Some(r)) => match r.parse::<f64>() {
                Ok(r) if (0.0..=40.0).contains(&r) => super::shadow_profile::set_corner(Some(r)),
                _ => return usage(),
            },
            ("--background", Some(g)) => match g.parse::<f64>() {
                Ok(g) if (0.0..=1.0).contains(&g) => super::shadow_profile::set_background(Some(g)),
                _ => return usage(),
            },
            ("--settle", Some(ms)) => match ms.parse::<u64>() {
                Ok(ms) => settle = Duration::from_millis(ms.clamp(100, 5000)),
                Err(_) => return usage(),
            },
            ("--size", Some(s)) => {
                let v: Vec<f64> = s.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let [w, h] = v[..] else { return usage() };
                if !(200.0..=2000.0).contains(&w) || !(150.0..=1500.0).contains(&h) {
                    return usage();
                }
                size = NSSize::new(w, h);
            }
            ("--only", Some("corner")) => only = Some(true),
            ("--only", Some("middle")) => only = Some(false),
            ("--stack", Some(n)) => match n.parse::<usize>() {
                Ok(n) if (1..=32).contains(&n) => stack = Some(n),
                _ => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
    let app = NSApplication::sharedApplication(mtm);
    // Never frontmost: the window stays inactive, as a parked window is.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let Some(main) = desktop::main_display(mtm) else {
        println!("{}", json!({ "ok": false, "why": "no main display" }));
        return ExitCode::FAILURE;
    };
    if let Some(n) = stack {
        return run_stack(mtm, &main, n, size, settle);
    }
    if profile || compare {
        return super::shadow_profile::run(mtm, &main, size, settle, dump.as_deref(), compare, appearance);
    }
    let mut variants = Vec::new();
    for toolbar in [false, true] {
        for corner in [true, false] {
            variants.push(Variant { toolbar, corner, normal_level: false });
        }
    }
    variants.push(Variant { toolbar: false, corner: true, normal_level: true });
    variants.retain(|v| only.is_none_or(|c| v.corner == c));
    let mut out = Vec::new();
    let mut suggested: Option<[f64; 4]> = None;
    for v in variants {
        let result = measure(mtm, &main, v, size, settle);
        if let Ok(m) = &result
            && m.drawn
            && let Some((r, _)) = m.reach.iter().find(|(t, _)| *t == SUGGESTED_AT).and_then(|(_, r)| r.as_ref())
        {
            let s = suggested.get_or_insert([0.0; 4]);
            for i in 0..4 {
                s[i] = s[i].max((r[i] as f64 / m.scale).ceil());
            }
        }
        out.push(match result {
            Ok(m) => m.json(&v),
            Err(why) => json!({ "variant": v.name(), "ok": false, "why": why }),
        });
    }
    let suggested = suggested.map(|[l, t, r, b]| {
        json!({
            "left": l, "top": t, "right": r, "bottom": b,
            "rust": format!("pub const DEFAULT_REACH: Reach = Reach {{ left: {l:.1}, top: {t:.1}, right: {r:.1}, bottom: {b:.1} }};"),
        })
    });
    println!(
        "{}",
        json!({
            "ok": suggested.is_some(),
            "display": main.frame,
            "threshold": SUGGESTED_AT,
            "suggested": suggested,
            "variants": out,
        })
    );
    if suggested.is_some() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// What one window's image showed.
struct Measured {
    listed: Rect,
    scale: f64,
    drawn: bool,
    middle_alpha: u8,
    /// Per threshold: the reach in pixels and whether it touched the image's edge.
    reach: Vec<(u8, Option<Reached>)>,
    /// The size (points) of the image the window server gives for the window and its framing.
    framing: Option<(f64, f64)>,
}

impl Measured {
    fn json(&self, v: &Variant) -> Value {
        let reach: serde_json::Map<String, Value> = self
            .reach
            .iter()
            .map(|(t, r)| {
                let value = match r {
                    Some((px, clipped)) => {
                        let pt = px.map(|p| p as f64 / self.scale);
                        json!({ "left": pt[0], "top": pt[1], "right": pt[2], "bottom": pt[3], "clipped": clipped })
                    }
                    None => Value::Null,
                };
                (t.to_string(), value)
            })
            .collect();
        json!({
            "variant": v.name(),
            "ok": true,
            "listed": self.listed,
            "scale": self.scale,
            "drawn": self.drawn,
            "middle_alpha": self.middle_alpha,
            "framing_size": self.framing.map(|(w, h)| json!({ "w": w, "h": h })),
            "reach": reach,
        })
    }
}

/// Opens window `v`, measures its shadow and closes it.
fn measure(mtm: MainThreadMarker, main: &crate::engine::model::Display, v: Variant, size: NSSize, settle: Duration) -> Result<Measured, String> {
    let window = make_window(mtm, v, size);
    let result = (|| {
        // Its place, global top-left: the park corner's (its top-left point showing, as a
        // parked window's), or the middle of the visible frame.
        let f = main.frame;
        let (x, y) = if v.corner {
            let x = if main.corner.right() { f.max_x() - 1.0 } else { f.x - size.width + 1.0 };
            let y = if main.corner.bottom() { f.max_y() - CORNER_SHOWS } else { main.visible.y };
            (x, y)
        } else {
            (main.visible.x + (main.visible.w - size.width) / 2.0, main.visible.y + (main.visible.h - size.height) / 2.0)
        };
        let primary = desktop::primary_height(mtm);
        let place = |w: &NSWindow| {
            let frame = w.frame();
            w.setFrame_display(NSRect::new(NSPoint::new(x, primary - y - frame.size.height), frame.size), false);
        };
        place(&window);
        window.orderFrontRegardless();
        // AppKit may keep a titled window's title bar on screen as it orders it in: placed
        // again once it is in.
        place(&window);
        spin(settle);
        let id = u32::try_from(window.windowNumber()).map_err(|_| "no window number".to_string())?;
        let listed = windows::describe(&[id]).into_iter().next().map(|w| w.bounds).ok_or("the window server does not list the window")?;
        if window.isKeyWindow() || NSApplication::sharedApplication(mtm).isActive() {
            return Err("the probe's window became active; not measured".into());
        }
        let grown = Rect::new(listed.x - MARGIN, listed.y - MARGIN, listed.w + 2.0 * MARGIN, listed.h + 2.0 * MARGIN);
        let image = window_image(id, Some(grown)).ok_or("no image of the window (CGWindowListCreateImage gave none)")?;
        let framing = window_image(id, None).map(|i| {
            let scale = objc2_core_graphics::CGImage::width(Some(&image)) as f64 / grown.w;
            (objc2_core_graphics::CGImage::width(Some(&i)) as f64 / scale, objc2_core_graphics::CGImage::height(Some(&i)) as f64 / scale)
        });
        let (bytes, w, h) = pixels::bgra(&image, None).ok_or("the image could not be read")?;
        let scale = w as f64 / grown.w;
        let frame = PixelRect {
            x: (MARGIN * scale).round() as usize,
            y: (MARGIN * scale).round() as usize,
            w: (listed.w * scale).round() as usize,
            h: (listed.h * scale).round() as usize,
        };
        let middle_alpha = middle_alpha(&bytes, w * 4, 3, frame);
        let reach = THRESHOLDS.iter().map(|t| (*t, reach_px(&bytes, w * 4, w, h, 3, frame, *t))).collect();
        Ok(Measured { listed, scale, drawn: middle_alpha >= 200, middle_alpha, reach, framing })
    })();
    window.orderOut(None);
    window.close();
    spin(Duration::from_millis(100));
    result
}

/// `--stack N`: the shadows of 1, 2, 4 … `n` titled windows at one frame in the middle, low
/// level (parked windows share the corner, and their faint shadow tails add up: his 14 parked
/// windows showed a shadow edge past the one-window reach, 10-30), taken together as one
/// image of the probe's own windows (`CGWindowListCreateImageFromArray`). Prints one JSON
/// object: per count, the reach per side at each threshold.
fn run_stack(mtm: MainThreadMarker, main: &crate::engine::model::Display, n: usize, size: NSSize, settle: Duration) -> ExitCode {
    let v = Variant { toolbar: false, corner: false, normal_level: false };
    let (x, y) = (main.visible.x + (main.visible.w - size.width) / 2.0, main.visible.y + (main.visible.h - size.height) / 2.0);
    let primary = desktop::primary_height(mtm);
    let mut windows: Vec<Retained<NSWindow>> = Vec::new();
    let mut rows = Vec::new();
    let mut counts: Vec<usize> = std::iter::successors(Some(1usize), |c| Some(c * 2)).take_while(|c| *c < n).collect();
    counts.push(n);
    for count in counts {
        while windows.len() < count {
            let w = make_window(mtm, v, size);
            let frame = w.frame();
            w.setFrame_display(NSRect::new(NSPoint::new(x, primary - y - frame.size.height), frame.size), false);
            w.orderFrontRegardless();
            w.setFrame_display(NSRect::new(NSPoint::new(x, primary - y - frame.size.height), frame.size), false);
            windows.push(w);
        }
        spin(settle);
        let row = (|| {
            if NSApplication::sharedApplication(mtm).isActive() || windows.iter().any(|w| w.isKeyWindow()) {
                return Err("a probe window became active; not measured".to_string());
            }
            let ids: Vec<u32> = windows.iter().filter_map(|w| u32::try_from(w.windowNumber()).ok()).collect();
            let listed = windows::describe(&ids[..1]).into_iter().next().map(|w| w.bounds).ok_or("the window server does not list the window")?;
            let grown = Rect::new(listed.x - MARGIN, listed.y - MARGIN, listed.w + 2.0 * MARGIN, listed.h + 2.0 * MARGIN);
            let image = stack_image(&ids, grown).ok_or("no image of the windows")?;
            let (bytes, w, h) = pixels::bgra(&image, None).ok_or("the image could not be read")?;
            let scale = w as f64 / grown.w;
            let frame = PixelRect {
                x: (MARGIN * scale).round() as usize,
                y: (MARGIN * scale).round() as usize,
                w: (listed.w * scale).round() as usize,
                h: (listed.h * scale).round() as usize,
            };
            let reach: serde_json::Map<String, Value> = THRESHOLDS
                .iter()
                .map(|t| {
                    let value = match reach_px(&bytes, w * 4, w, h, 3, frame, *t) {
                        Some((px, clipped)) => {
                            let pt = px.map(|p| p as f64 / scale);
                            json!({ "left": pt[0], "top": pt[1], "right": pt[2], "bottom": pt[3], "clipped": clipped })
                        }
                        None => Value::Null,
                    };
                    (t.to_string(), value)
                })
                .collect();
            Ok(json!({ "windows": count, "ok": true, "reach": reach }))
        })();
        rows.push(row.unwrap_or_else(|why| json!({ "windows": count, "ok": false, "why": why })));
    }
    for w in &windows {
        w.orderOut(None);
        w.close();
    }
    spin(Duration::from_millis(100));
    let ok = rows.iter().all(|r| r["ok"] == true);
    println!("{}", json!({ "ok": ok, "display": main.frame, "stack": rows }));
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The image of the probe's own windows `ids` together, with their framing, over `bounds`
/// (global, top-left), at the display's resolution.
fn stack_image(ids: &[u32], bounds: Rect) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
    // The array holds the CGWindowIDs themselves as its values (no callbacks), as the call
    // expects ([`windows::describe`]).
    let mut values: Vec<*const std::ffi::c_void> = ids.iter().map(|&id| id as usize as *const std::ffi::c_void).collect();
    let count = objc2_core_foundation::CFIndex::try_from(values.len()).ok()?;
    // SAFETY: `values` holds `count` pointer-sized values and outlives the call; null
    // callbacks make CF store them as they are, never retained or released.
    let array = unsafe { objc2_core_foundation::CFArray::new(None, values.as_mut_ptr(), count, std::ptr::null()) }?;
    let rect = CGRect::new(CGPoint::new(bounds.x, bounds.y), CGSize::new(bounds.w, bounds.h));
    // SAFETY: an array of CGWindowIDs, as the call is documented to take; only the probe's
    // own windows are named, so nothing else is captured.
    #[allow(deprecated)]
    unsafe {
        objc2_core_graphics::CGWindowListCreateImageFromArray(rect, &array, CGWindowImageOption::BestResolution)
    }
}

/// A titled window (with a unified toolbar for `v.toolbar`), at a level just above the
/// desktop icons (under every normal window) unless `v.normal_level`; never key.
pub(super) fn make_window(mtm: MainThreadMarker, v: Variant, size: NSSize) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable;
    // SAFETY: a fresh allocation, a valid frame and style; released only by us.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), size), style, NSBackingStoreType::Buffered, false)
    };
    // SAFETY: the window is owned by this function's caller until closed.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Shadow probe"));
    if v.toolbar {
        let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str("dev.eugene.stagemanager.shadow-probe"));
        window.setToolbar(Some(&toolbar));
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);
    }
    if !v.normal_level {
        window.setLevel((CGWindowLevelForKey(CGWindowLevelKey::DesktopIconWindowLevelKey) + 1) as isize);
    }
    window
}

/// The image of window `id` alone with its framing, over `bounds` (global, top-left), or over
/// the window and its framing when `None`, at the display's resolution.
pub(super) fn window_image(id: u32, bounds: Option<Rect>) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
    // CGRectNull: the window's own bounds, framing included.
    let rect = bounds.map_or(CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::new(0.0, 0.0)), |b| {
        CGRect::new(CGPoint::new(b.x, b.y), CGSize::new(b.w, b.h))
    });
    // Our own window only (kCGWindowListOptionIncludingWindow): no screen and no other
    // app's window is captured. Deprecated in favour of ScreenCaptureKit, which would need
    // Screen Recording for the process that runs the probe.
    #[allow(deprecated)]
    objc2_core_graphics::CGWindowListCreateImage(rect, CGWindowListOption::OptionIncludingWindow, id, CGWindowImageOption::BestResolution)
}

/// Runs the main run loop for `d` (the window server draws the window meanwhile).
pub(super) fn spin(d: Duration) {
    let until = NSDate::dateWithTimeIntervalSinceNow(d.as_secs_f64());
    NSRunLoop::currentRunLoop().runUntilDate(&until);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` x `h` BGRA image, alpha from `a(x, y)`.
    fn image(w: usize, h: usize, a: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[0, 0, 0, a(x, y)]);
            }
        }
        out
    }

    #[test]
    fn the_reach_is_how_far_the_alpha_reaches_past_each_side_of_the_frame() {
        // A 100x80 image, the frame at 30,20 40x30, a shadow 5 px left and right, 3 above and
        // 12 below, fading: 1 at its far edge, 10 nearer.
        let frame = PixelRect { x: 30, y: 20, w: 40, h: 30 };
        let shadow = |x: usize, y: usize| -> u8 {
            let inside_x = (25..75).contains(&x);
            let inside_y = (17..62).contains(&y);
            if (30..70).contains(&x) && (20..50).contains(&y) {
                255
            } else if inside_x && inside_y {
                if x == 25 || x == 74 || y == 17 || y == 61 { 1 } else { 10 }
            } else {
                0
            }
        };
        let px = image(100, 80, shadow);
        assert_eq!(reach_px(&px, 400, 100, 80, 3, frame, 1), Some(([5, 3, 5, 12], false)));
        assert_eq!(reach_px(&px, 400, 100, 80, 3, frame, 2), Some(([4, 2, 4, 11], false)), "the faint edge left out");
        assert_eq!(reach_px(&px, 400, 100, 80, 3, frame, 200), Some(([0, 0, 0, 0], false)), "the window alone");
        assert_eq!(middle_alpha(&px, 400, 3, frame), 255);
        let clear = image(100, 80, |_, _| 0);
        assert_eq!(reach_px(&clear, 400, 100, 80, 3, frame, 1), None);
        assert_eq!(middle_alpha(&clear, 400, 3, frame), 0, "not drawn");
        // A shadow reaching the image's edge: the margin was too small, said so.
        let wide = image(100, 80, |x, y| if (20..50).contains(&y) { if (30..70).contains(&x) { 255 } else { 3 } } else { 0 });
        assert_eq!(reach_px(&wide, 400, 100, 80, 3, frame, 2), Some(([30, 0, 30, 0], true)));
    }
}
