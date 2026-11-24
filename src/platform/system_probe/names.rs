//! `probe-spaces names [NORMAL-BUNDLE] [--sticky BUNDLE] [--window ID] [--calls N]` (probe 3,
//! the review's R3; read only, no window moved): what the window server calls the screens and
//! the windows' Desktops.
//!
//! - Each entry of `SLSCopyManagedDisplaySpaces` in its order: its "Display Identifier" (the
//!   word `Main`, a uuid, or something else: values are not printed, only their shape) and
//!   which connected display it names (by the display's ColorSync UUID, or `Main`), its
//!   "Current Space", and each Space's id, type, whether it has a `uuid` key and the key's
//!   length, and the other keys it carries (names only).
//! - Whether "Displays have separate Spaces" is on (`NSScreen.screensHaveSeparateSpaces`, and
//!   `com.apple.spaces` `spans-displays` as written), and which screen `SLSGetActiveSpace`
//!   names.
//! - `SLSCopySpacesForWindows` (masks 0x7, as yabai passes it, and 0xf, as the engine does)
//!   for a normal fixture window (NORMAL-BUNDLE's first), for a window on every Desktop
//!   (`--sticky BUNDLE`: a fixture started with `--all-spaces`, its first window joins every
//!   Space), and for a window set to "All Desktops" by hand in the Dock's menu (`--window ID`:
//!   the Dock's menu is not scripted here, the probe prints how to set it).
//! - The cost of one `SLSCopyManagedDisplaySpaces` (the call and its release; `--calls` times,
//!   default 2000), of the same with the engine's parse ([`Sky::displays`]), and of one
//!   `SLSGetActiveSpace`: median, 99th percentile and worst, in µs.

use std::time::Instant;

use objc2::rc::Retained;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CFBoolean, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFString};
use objc2_core_graphics::{CGGetActiveDisplayList, CGMainDisplayID};
use objc2_foundation::{NSArray, NSDictionary, NSString};

use super::{Args, Ctx, fixture, main_entry, median, quantile, table, worst};
use crate::platform::desktop_counts;
use crate::platform::spaces::ffi::{self, Sky, wid_array};

/// What an identifier looks like, never its value (`Main` is printed: it is a word).
fn shape(id: &str) -> String {
    let uuid = id.len() == 36 && id.chars().filter(|c| *c == '-').count() == 4 && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    if id == "Main" {
        "the word Main".into()
    } else if uuid {
        "a uuid (36 chars)".into()
    } else if id.is_empty() {
        "empty".into()
    } else {
        format!("something else ({} chars)", id.chars().count())
    }
}

/// The keys of `d`, sorted.
fn keys(d: &NSDictionary) -> Vec<String> {
    let mut k: Vec<String> = d.allKeys().iter().filter_map(|k| k.downcast::<NSString>().ok()).map(|s| s.to_string()).collect();
    k.sort();
    k
}

/// `d[key]` as a string's length, `None` when absent or not a string.
fn string_len(d: &NSDictionary, key: &str) -> Option<usize> {
    ffi::get(d, key).and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string().chars().count())
}

/// The active displays: (CGDirectDisplayID, ColorSync UUID, main).
fn active_displays() -> Vec<(u32, Option<String>, bool)> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: `ids` holds 16 entries and both pointers are valid for the call.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    let main = CGMainDisplayID();
    if err.0 != 0 {
        return vec![(main, desktop_counts::display_uuid(main), true)];
    }
    ids.iter().take(count as usize).map(|&id| (id, desktop_counts::display_uuid(id), id == main)).collect()
}

/// `com.apple.spaces` `spans-displays` as written (true: one set of Spaces across screens).
fn spans_displays() -> Option<bool> {
    let domain = CFString::from_static_str("com.apple.spaces");
    CFPreferencesAppSynchronize(&domain);
    let v = CFPreferencesCopyAppValue(&CFString::from_static_str("spans-displays"), &domain)?;
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool());
    }
    v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()).map(|n| n != 0)
}

/// `SLSCopySpacesForWindows(cid, mask, [id])`.
fn spaces_for(sky: &Sky, id: u32, mask: i32) -> Option<Vec<u64>> {
    let f = sky.spaces_for_windows?;
    let arr = wid_array(&[id]);
    // SAFETY: a CFArray of one window number that outlives the call; a +1 CFArray of CFNumber or null.
    unsafe { ffi::take_array(f(sky.cid, mask, Retained::as_ptr(&arr).cast())) }.map(|a| ffi::numbers(&a))
}

/// Microseconds of `n` runs of `f`: (median, 99th percentile, worst).
fn cost(n: usize, mut f: impl FnMut()) -> (f64, f64, f64) {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        objc2::rc::autoreleasepool(|_| {
            let t = Instant::now();
            f();
            v.push(t.elapsed().as_secs_f64() * 1e6);
        });
    }
    (median(&v).unwrap_or(0.0), quantile(&v, 0.99).unwrap_or(0.0), worst(&v).unwrap_or(0.0))
}

pub(super) fn run(ctx: &Ctx, args: &Args) -> Result<(), String> {
    let sky = &ctx.sky;
    let f = sky.copy_managed.ok_or("SLSCopyManagedDisplaySpaces is missing on this macOS")?;
    println!("== names: what the window server calls the screens and the windows' Desktops (read only) ==");
    let displays = active_displays();
    let main_uuid = displays.iter().find(|d| d.2).and_then(|d| d.1.clone());
    println!("  connected displays: {} (the main one {})", displays.len(), if main_uuid.is_some() { "has a ColorSync UUID" } else { "has NO ColorSync UUID" });
    // SAFETY: returns a +1 CFArray of CFDictionary (or null).
    let list: Retained<NSArray> = unsafe { ffi::take_array(f(sky.cid)) }.ok_or("SLSCopyManagedDisplaySpaces returned nothing")?;
    let entries: Vec<Retained<NSDictionary>> = list.iter().filter_map(|d| d.downcast::<NSDictionary>().ok()).collect();
    let active = sky.active_space();
    let mut rows = Vec::new();
    let mut main_at = None;
    let mut ids: Vec<String> = Vec::new();
    for (i, d) in entries.iter().enumerate() {
        let id = ffi::get(d, "Display Identifier").and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
        let names = displays.iter().position(|(_, u, _)| u.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(&id)));
        let which = match names {
            Some(k) if displays[k].2 => "the main display (by its UUID)".to_string(),
            Some(k) => format!("display {} of {} (by its UUID)", k + 1, displays.len()),
            None if id == "Main" => "the main display (Main)".into(),
            None => "no connected display".into(),
        };
        if which.starts_with("the main display") {
            main_at.get_or_insert(i);
        }
        let current = ffi::get(d, "Current Space").and_then(|v| v.downcast::<NSDictionary>().ok());
        let current_id = current.as_ref().and_then(|c| ffi::number(c, "id64"));
        let spaces = ffi::get(d, "Spaces").and_then(|v| v.downcast::<NSArray>().ok()).map(|a| a.iter().filter_map(|s| s.downcast::<NSDictionary>().ok()).collect::<Vec<_>>()).unwrap_or_default();
        rows.push(vec![
            i.to_string(),
            shape(&id),
            which,
            current_id.map_or("-".into(), |c| c.to_string()),
            spaces.len().to_string(),
            if active.is_some() && active == current_id { "yes".into() } else { String::new() },
            keys(d).join(","),
        ]);
        ids.push(id);
        println!("  entry {i}: its Spaces");
        let space_rows: Vec<Vec<String>> = spaces
            .iter()
            .map(|s| {
                let sid = ffi::number(s, "id64");
                let uuid = match string_len(s, "uuid") {
                    Some(0) => "present, empty".to_string(),
                    Some(n) => format!("present, {n} chars"),
                    None if ffi::get(s, "uuid").is_some() => "present, not a string".into(),
                    None => "absent".into(),
                };
                let managed = ffi::number(s, "ManagedSpaceID");
                vec![
                    sid.map_or("-".into(), |v| v.to_string()),
                    ffi::number(s, "type").map_or("-".into(), |v| v.to_string()),
                    uuid,
                    match (managed, sid) {
                        (Some(m), Some(s)) if m == s => "= id".into(),
                        (Some(m), _) => m.to_string(),
                        (None, _) => "absent".into(),
                    },
                    if sid.is_some() && sid == current_id { "current".into() } else { String::new() },
                    keys(s).join(","),
                ]
            })
            .collect();
        table(&["space", "type", "uuid key", "ManagedSpaceID", "", "keys"], &space_rows);
        if let Some(c) = &current {
            println!("    Current Space: id {}, type {}, uuid {}", current_id.map_or("-".into(), |v| v.to_string()), ffi::number(c, "type").map_or("-".into(), |v| v.to_string()), string_len(c, "uuid").map_or("absent".into(), |n| format!("{n} chars")));
        }
    }
    println!("\n  SLSCopyManagedDisplaySpaces, in its order (type 0 is a Desktop):");
    table(&["entry", "Display Identifier", "names", "Current Space", "spaces", "SLSGetActiveSpace", "keys"], &rows);
    let separate = NSScreen::screensHaveSeparateSpaces(ctx.mtm);
    println!(
        "\n  \"Displays have separate Spaces\": {} (NSScreen); com.apple.spaces spans-displays: {}",
        if separate { "on" } else { "off" },
        spans_displays().map_or("absent (the default: separate)".into(), |b| b.to_string())
    );
    println!("  SLSGetActiveSpace: {}", active.map_or("missing or 0".into(), |a| a.to_string()));
    let by_engine = main_entry(&sky.displays());

    // The windows.
    let desktops: Vec<(usize, u64)> = entries
        .iter()
        .enumerate()
        .flat_map(|(i, d)| {
            ffi::get(d, "Spaces")
                .and_then(|v| v.downcast::<NSArray>().ok())
                .map(|a| a.iter().filter_map(|s| s.downcast::<NSDictionary>().ok()).filter(|s| ffi::number(s, "type") == Some(0)).filter_map(|s| ffi::number(&s, "id64")).map(|s| (i, s)).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect();
    let currents: Vec<u64> = entries.iter().filter_map(|d| ffi::get(d, "Current Space").and_then(|v| v.downcast::<NSDictionary>().ok()).and_then(|c| ffi::number(&c, "id64"))).collect();
    let says = |v: &[u64]| -> String {
        if v.is_empty() {
            return "no Space".into();
        }
        let all: Vec<u64> = desktops.iter().map(|d| d.1).collect();
        let mut sorted = v.to_vec();
        sorted.sort_unstable();
        let mut every = all.clone();
        every.sort_unstable();
        if sorted == every && all.len() > 1 {
            return format!("every Desktop of every screen ({})", v.len());
        }
        let main_desks: Vec<u64> = desktops.iter().filter(|d| Some(d.0) == main_at).map(|d| d.1).collect();
        let mut m = main_desks.clone();
        m.sort_unstable();
        if sorted == m && m.len() > 1 {
            return format!("every Desktop of the main screen ({})", v.len());
        }
        if v.len() == 1 && currents.contains(&v[0]) {
            return "the Desktop shown (one Space)".into();
        }
        if v.len() == 1 && all.contains(&v[0]) {
            return "one Desktop, not the one shown".into();
        }
        format!("{} Spaces, {} of them Desktops", v.len(), v.iter().filter(|s| all.contains(s)).count())
    };
    let mut windows: Vec<(String, u32)> = Vec::new();
    if let Some(b) = args.bundles.first() {
        match fixture(sky, b).1.first() {
            Some(w) => windows.push(("a normal fixture window".into(), w.id)),
            None => println!("  {b}: no window found"),
        }
    }
    if let Some(b) = &args.sticky {
        match fixture(sky, b).1.first() {
            Some(w) => windows.push(("a fixture window joining every Space (--all-spaces)".into(), w.id)),
            None => println!("  {b}: no window found"),
        }
    }
    match args.window {
        Some(id) => windows.push(("the window set to All Desktops by hand".into(), id)),
        None => println!(
            "  a window set to \"All Desktops\" in the Dock's menu: not measured (the Dock's menu is not scripted). By hand: right-click a fixture's Dock item, Options, Assign To: All Desktops; read its id with `stage-manager probe-windows <bundle id>` and run again with --window ID; set it back to None after"
        ),
    }
    let mut window_rows = Vec::new();
    for (what, id) in &windows {
        let seven = spaces_for(sky, *id, 0x7);
        let fifteen = spaces_for(sky, *id, 0xf);
        window_rows.push(vec![
            what.clone(),
            seven.as_deref().map_or("call failed".into(), says),
            fifteen.as_deref().map_or("call failed".into(), says),
            fifteen.map_or("-".into(), |v| format!("{v:?}")),
        ]);
    }
    if !window_rows.is_empty() {
        println!("\n  SLSCopySpacesForWindows ({} Desktops in all):", desktops.len());
        table(&["window", "mask 0x7", "mask 0xf", "the ids (0xf)"], &window_rows);
    }

    // The cost.
    let n = args.calls;
    let raw = cost(n, || {
        // SAFETY: as above; the +1 array is released as it drops.
        drop(std::hint::black_box(unsafe { ffi::take_array(f(sky.cid)) }));
    });
    let parsed = cost(n, || drop(std::hint::black_box(sky.displays())));
    let act = cost(n, || {
        std::hint::black_box(sky.active_space());
    });
    let spaces_of = windows.first().map(|(_, id)| cost(n, || drop(std::hint::black_box(sky.spaces_of(&[*id])))));
    println!("\n  cost, µs over {n} calls each:");
    let row = |what: &str, c: (f64, f64, f64)| vec![what.to_string(), format!("{:.1}", c.0), format!("{:.1}", c.1), format!("{:.1}", c.2)];
    let mut cost_rows = vec![
        row("SLSCopyManagedDisplaySpaces (call + release)", raw),
        row("the same with the engine's parse (Sky::displays)", parsed),
        row("SLSGetActiveSpace", act),
    ];
    if let Some(c) = spaces_of {
        cost_rows.push(row("SLSCopySpacesForWindows, one window", c));
    }
    table(&["call", "median", "p99", "worst"], &cost_rows);

    println!();
    let main_id = main_at.and_then(|i| ids.get(i));
    println!(
        "VERDICT main screen: its entry's Display Identifier is {}; it is entry {} of {}{}",
        main_id.map_or("NOT FOUND".into(), |s| shape(s)),
        main_at.map_or("-".into(), |i| i.to_string()),
        entries.len(),
        if main_at == Some(0) { " (main first)" } else { " (NOT first)" }
    );
    println!(
        "VERDICT screen ids: {} (a map keyed by the Display Identifier sees the main screen go only if it is a uuid; \"Main\" names whichever screen is main)",
        if ids.iter().all(|i| i == "Main" || shape(i).starts_with("a uuid")) { "Main or uuids only" } else { "something else too" }
    );
    println!("VERDICT the probe's main-entry rule (Main, else the main display's UUID) finds entry {}", by_engine.map_or("none".into(), |i| i.to_string()));
    let uuids: Vec<Option<usize>> = entries
        .iter()
        .flat_map(|d| ffi::get(d, "Spaces").and_then(|v| v.downcast::<NSArray>().ok()).map(|a| a.iter().filter_map(|s| s.downcast::<NSDictionary>().ok()).map(|s| string_len(&s, "uuid")).collect::<Vec<_>>()).unwrap_or_default())
        .collect();
    let empty = uuids.iter().filter(|u| **u == Some(0)).count();
    let absent = uuids.iter().filter(|u| u.is_none()).count();
    println!("VERDICT Space uuids: {} of {} Spaces carry a non-empty uuid, {empty} an empty one, {absent} none", uuids.iter().filter(|u| u.is_some_and(|n| n > 0)).count(), uuids.len());
    println!("VERDICT separate Spaces per display: {}", if separate { "on" } else { "off" });
    for r in &window_rows {
        println!("VERDICT {}: lists {} (mask 0xf)", r[0], r[2]);
    }
    println!("VERDICT cost: one SLSCopyManagedDisplaySpaces takes a median {:.0} µs, worst {:.0} µs (with the parse {:.0} / {:.0} µs)", raw.0, raw.2, parsed.0, parsed.2);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_told_by_their_shape() {
        assert_eq!(shape("Main"), "the word Main");
        assert_eq!(shape("37D8832A-2D66-02CA-B9F7-8F30A301B230"), "a uuid (36 chars)");
        assert_eq!(shape(""), "empty");
        assert_eq!(shape("abc"), "something else (3 chars)");
    }

    #[test]
    fn costs_have_three_figures() {
        let (m, p, w) = cost(20, || {});
        assert!(m <= p && p <= w);
    }
}
