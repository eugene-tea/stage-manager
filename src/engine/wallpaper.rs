//! The desktop picture under the corner patch, drawn from its file ([`super::corner_patch`]):
//! which picture and frame macOS shows, where it lands on the screen, and which part of it
//! the patch covers. Pure: the platform layer ([`crate::platform::wallpaper`]) reads the
//! facts (NSWorkspace, the picture's ImageIO metadata, macOS's wallpaper settings file, the
//! appearance, the clock) and draws what this module decides. No screen capture anywhere.
//!
//! - **Placement** ([`Placement`]): NSWorkspace's `desktopImageOptionsForScreen:` names the
//!   scaling (`NSImageScaling`) and whether clipping is allowed: proportional up or down
//!   with clipping is Fill, without it Fit; independent axes Stretch; none Center. Tile has
//!   no NSWorkspace form; macOS's wallpaper settings name it (`placement`, [`StoreChoice`]).
//!   Fill and Fit scale the picture about the screen's center; Stretch spans the screen;
//!   Center and Tile draw it at its own size in points (72 dpi: a pixel a point), Center
//!   about the screen's center, Tile from the top-left. Gaps show the fill color.
//! - **Frame** ([`frame`]): a dynamic HEIC carries `apple_desktop` metadata ([`schedule`]):
//!   `apr` (light and dark frames), `h24` (frames by the time of day), `solar` (frames by
//!   the sun's place in the sky). Which one shows depends on the style picked in System
//!   Settings (Automatic, Dynamic, Light, Dark: [`Style`]), which only macOS's wallpaper
//!   settings file holds; without it a dynamic picture is not drawn. Solar frames need the
//!   sun's position, so a place: without Location Services, the time zone's own place
//!   (`/usr/share/zoneinfo/zone.tab`), and only while every place within
//!   [`SOLAR_SLACK_DEG`] of it would show the same frame ([`solar_frame`]).
//! - **What is never drawn** ([`decide`]): videos and aerials, shuffles (a folder, Photos),
//!   a system wallpaper description (`.madesktop`), a picture macOS's settings do not name
//!   for the screen, Desktops with this picture set to different styles: the patch stays off.
//!   Exact or off: Center and Tile (their rule, the picture at its own size in points, is
//!   not checked against the screen yet) and Fit with no fill colour (what macOS shows
//!   around the picture then is unchecked) stay off until a live check covers them. With
//!   macOS's settings unreadable, only NSWorkspace's Fill, Fit and Stretch are taken.
//! - **Crossfades.** macOS crossfades a dynamic picture from one frame to the next (light to
//!   dark, an h24 or solar boundary) over about a second; the patch cuts from frame to frame
//!   at the boundary (it hides while the new frame renders), so for that second the corner
//!   differs from the rest of the screen. Not modelled: no public API tells the fade.
//! - **Region** ([`region`], [`crop_px`]): the picture is rendered once for a block of the
//!   corner ([`BLOCK`], grown to hold the patch) and the patch shows a pixel-aligned crop of
//!   it, so a rectangle that changes needs no new render.

use super::geometry::{Rect, Size};

// ----- property list values ------------------------------------------------------------

/// A property list value as the platform layer reads it (binary plists inside `data`
/// values already parsed in place).
#[derive(Debug, Clone, PartialEq)]
pub enum Plist {
    /// A dictionary, keys in file order.
    Dict(Vec<(String, Plist)>),
    /// An array.
    Array(Vec<Plist>),
    /// A string.
    Str(String),
    /// A number (integers, reals and booleans alike).
    Num(f64),
    /// Data that is not a property list.
    Data(Vec<u8>),
    /// Anything else (dates, null).
    Other,
}

impl Plist {
    /// The value at `key` of a dictionary.
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at a path of dictionary keys.
    pub fn at(&self, keys: &[&str]) -> Option<&Plist> {
        keys.iter().try_fold(self, |p, k| p.get(k))
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The number, if this is a finite one (a numeric string counts: `a = "-25"`).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Plist::Num(n) if n.is_finite() => Some(*n),
            Plist::Str(s) => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
            _ => None,
        }
    }

    /// The number as a frame index (a whole number, 0 or more).
    pub fn as_index(&self) -> Option<usize> {
        self.as_f64().filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n < 1e6).map(|n| n as usize)
    }

    /// The elements, if this is an array.
    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// Decodes standard base64 (the `apple_desktop` metadata's form); `None` on any bad input.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body = bytes.strip_suffix(b"==").or_else(|| bytes.strip_suffix(b"=")).unwrap_or(&bytes);
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in body {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ----- placement -------------------------------------------------------------------------

/// How the picture fills the screen (System Settings → Wallpaper).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Scaled to cover the screen, centered, the overflow cut ("Fill Screen").
    Fill,
    /// Scaled to fit inside the screen, centered, the fill color around it ("Fit to Screen").
    Fit,
    /// Scaled on each axis to the screen ("Stretch to Fill Screen").
    Stretch,
    /// At its own size, centered ("Center").
    Center,
    /// At its own size, repeated from the top-left ("Tile").
    Tile,
}

impl Placement {
    /// From NSWorkspace's options: `NSWorkspaceDesktopImageScalingKey` (an `NSImageScaling`;
    /// absent: proportionally up or down) and `NSWorkspaceDesktopImageAllowClippingKey`
    /// (absent: no). `None` for a scaling with no System Settings form (proportionally down).
    pub fn from_workspace(scaling: Option<i64>, clipping: Option<bool>) -> Option<Self> {
        match (scaling.unwrap_or(3), clipping.unwrap_or(false)) {
            (3, true) => Some(Placement::Fill),
            (3, false) => Some(Placement::Fit),
            (1, _) => Some(Placement::Stretch),
            (2, _) => Some(Placement::Center),
            _ => None,
        }
    }

    /// From macOS's wallpaper settings (`placement` picker id: `Crop`, `Fit`, `Stretch`,
    /// `Center`, `Tile`).
    pub fn from_store(id: &str) -> Option<Self> {
        match id.to_ascii_lowercase().as_str() {
            "crop" | "fill" => Some(Placement::Fill),
            "fit" => Some(Placement::Fit),
            "stretch" => Some(Placement::Stretch),
            "center" | "centre" => Some(Placement::Center),
            "tile" => Some(Placement::Tile),
            _ => None,
        }
    }

    /// Its name, for logs and the probe.
    pub fn name(self) -> &'static str {
        match self {
            Placement::Fill => "fill",
            Placement::Fit => "fit",
            Placement::Stretch => "stretch",
            Placement::Center => "center",
            Placement::Tile => "tile",
        }
    }
}

/// The picture's size in pixels and in points (its resolution: 72 dpi is a point a pixel).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageSize {
    /// Width, pixels.
    pub px_w: f64,
    /// Height, pixels.
    pub px_h: f64,
    /// Width, points.
    pub pt_w: f64,
    /// Height, points.
    pub pt_h: f64,
}

impl ImageSize {
    /// From pixels and the resolution ImageIO reports (absent or odd: 72 dpi).
    pub fn new(px_w: f64, px_h: f64, dpi_w: Option<f64>, dpi_h: Option<f64>) -> Self {
        let dpi = |d: Option<f64>| d.filter(|d| d.is_finite() && *d >= 1.0).unwrap_or(72.0);
        Self { px_w, px_h, pt_w: px_w * 72.0 / dpi(dpi_w), pt_h: px_h * 72.0 / dpi(dpi_h) }
    }

    fn usable(&self) -> bool {
        [self.px_w, self.px_h, self.pt_w, self.pt_h].iter().all(|v| v.is_finite() && *v >= 1.0)
    }
}

/// Where the picture lands on a screen of `screen` points (screen-local, top-left): one
/// rectangle, or for [`Placement::Tile`] every tile that reaches `area`.
pub fn placed(placement: Placement, screen: Size, image: ImageSize, area: Rect) -> Vec<Rect> {
    if !image.usable() || !(screen.w > 0.0 && screen.h > 0.0) {
        return Vec::new();
    }
    let centered = |w: f64, h: f64| Rect::new((screen.w - w) / 2.0, (screen.h - h) / 2.0, w, h);
    match placement {
        Placement::Fill | Placement::Fit => {
            let (sx, sy) = (screen.w / image.px_w, screen.h / image.px_h);
            // The axis that fits takes the screen's own length, exactly.
            let by_width = (placement == Placement::Fill) == (sx >= sy);
            let (w, h) = if by_width { (screen.w, screen.w * image.px_h / image.px_w) } else { (screen.h * image.px_w / image.px_h, screen.h) };
            vec![centered(w, h)]
        }
        Placement::Stretch => vec![Rect::new(0.0, 0.0, screen.w, screen.h)],
        Placement::Center => vec![centered(image.pt_w, image.pt_h)],
        Placement::Tile => {
            let (tw, th) = (image.pt_w, image.pt_h);
            let cols = (area.x / tw).floor().max(0.0) as i64..=(area.max_x() / tw).ceil() as i64;
            let rows = (area.y / th).floor().max(0.0) as i64..=(area.max_y() / th).ceil() as i64;
            let mut out = Vec::new();
            for r in rows {
                for c in cols.clone() {
                    let t = Rect::new(c as f64 * tw, r as f64 * th, tw, th);
                    if t.intersect(&area).is_some() && t.x < screen.w && t.y < screen.h {
                        out.push(t);
                    }
                }
            }
            out
        }
    }
}

/// One draw of the whole picture: it lands on `dst` (points, relative to the area's
/// top-left corner), most of it outside the area and clipped there.
///
/// Always the whole picture, never a crop of it: Core Graphics resamples a cropped source
/// differently at high quality (up to 16 levels off on the owner's wallpaper, checked
/// against a whole-screen draw), while the whole picture drawn into an area-sized bitmap
/// gives the whole-screen draw's pixels exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Draw {
    /// Destination, points from the area's origin.
    pub dst: Rect,
}

/// The draws that paint `area` (screen-local points) as the whole screen shows it: each
/// placed picture reaching the area, moved to the area's coordinates.
pub fn draws(placement: Placement, screen: Size, image: ImageSize, area: Rect) -> Vec<Draw> {
    placed(placement, screen, image, area)
        .into_iter()
        .filter(|d| d.intersect(&area).is_some())
        .map(|d| Draw { dst: d.translate(-area.x, -area.y) })
        .collect()
}

/// True when one of `draws` covers the whole of an area of `size` (points): nothing of the
/// background shows through.
pub fn covered(draws: &[Draw], size: Size) -> bool {
    let area = Rect::new(0.0, 0.0, size.w, size.h);
    draws.iter().any(|d| d.dst.contains_rect(&area))
}

/// The block of the corner rendered at once (points): wider and taller than any patch the
/// default reach makes, so most rectangles are crops of one render.
pub const BLOCK: Size = Size::new(480.0, 560.0);

/// The screen-local region to render for a patch at `patch` (screen-local): the bottom-right
/// [`BLOCK`] grown to hold the patch, on the screen, in whole points.
pub fn region(screen: Size, patch: Rect) -> Rect {
    let block = Rect::new(screen.w - BLOCK.w, screen.h - BLOCK.h, BLOCK.w, BLOCK.h);
    let r = block.union(&patch);
    let (x, y) = (r.x.floor().max(0.0), r.y.floor().max(0.0));
    let (mx, my) = (r.max_x().ceil().min(screen.w), r.max_y().ceil().min(screen.h));
    Rect::new(x, y, (mx - x).max(0.0), (my - y).max(0.0))
}

/// The patch's pixels inside a render of `region` at `scale` pixels a point: whole pixels
/// (top-left), `None` unless the region holds the patch.
pub fn crop_px(region: Rect, patch: Rect, scale: f64) -> Option<Rect> {
    if !region.contains_rect(&patch) || !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let x0 = ((patch.x - region.x) * scale).round();
    let y0 = ((patch.y - region.y) * scale).round();
    let x1 = ((patch.max_x() - region.x) * scale).round();
    let y1 = ((patch.max_y() - region.y) * scale).round();
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// `rect` (global, top-left) relative to `display`'s top-left corner.
pub fn local(rect: Rect, display: Rect) -> Rect {
    rect.translate(-display.x, -display.y)
}

// ----- dynamic pictures ------------------------------------------------------------------

/// A frame chosen by the sun's place: `altitude` and `azimuth` in degrees (azimuth from
/// north, clockwise).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunFrame {
    /// Degrees above the horizon.
    pub altitude: f64,
    /// Degrees from north, clockwise.
    pub azimuth: f64,
    /// The frame.
    pub index: usize,
}

/// Which frame of a picture shows when.
#[derive(Debug, Clone, PartialEq)]
pub enum Schedule {
    /// One picture (the primary frame).
    Still,
    /// Light and dark frames (`apple_desktop:apr`).
    Appearance {
        /// The light frame.
        light: usize,
        /// The dark frame.
        dark: usize,
    },
    /// Frames by the time of day (`apple_desktop:h24`): from each `(fraction of the day, frame)`
    /// to the next; light and dark stills when given (`ap`).
    TimeOfDay {
        /// The light still.
        light: Option<usize>,
        /// The dark still.
        dark: Option<usize>,
        /// Sorted by the time.
        times: Vec<(f64, usize)>,
    },
    /// Frames by the sun (`apple_desktop:solar`); light and dark stills when given (`ap`).
    Solar {
        /// The light still.
        light: Option<usize>,
        /// The dark still.
        dark: Option<usize>,
        /// The frames by the sun's place.
        suns: Vec<SunFrame>,
    },
}

impl Schedule {
    /// Its kind, for logs and the probe.
    pub fn kind(&self) -> &'static str {
        match self {
            Schedule::Still => "still",
            Schedule::Appearance { .. } => "light and dark",
            Schedule::TimeOfDay { .. } => "time of day",
            Schedule::Solar { .. } => "solar",
        }
    }
}

fn light_dark(p: &Plist) -> (Option<usize>, Option<usize>) {
    (p.get("l").and_then(Plist::as_index), p.get("d").and_then(Plist::as_index))
}

/// The schedule from a picture's `apple_desktop` metadata (each value already decoded from
/// its base64 binary plist): solar first, then time of day, then light and dark; none is a
/// still.
pub fn schedule(apr: Option<&Plist>, h24: Option<&Plist>, solar: Option<&Plist>) -> Result<Schedule, &'static str> {
    if let Some(s) = solar {
        let (light, dark) = s.get("ap").map(light_dark).unwrap_or_default();
        let suns: Option<Vec<SunFrame>> = s.get("si").and_then(Plist::as_array).map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some(SunFrame {
                        altitude: e.get("a")?.as_f64()?,
                        azimuth: e.get("z")?.as_f64()?,
                        index: e.get("i")?.as_index()?,
                    })
                })
                .collect()
        });
        return match suns {
            Some(suns) if !suns.is_empty() => Ok(Schedule::Solar { light, dark, suns }),
            _ => Err("the picture's solar metadata names no frames"),
        };
    }
    if let Some(h) = h24 {
        let (light, dark) = h.get("ap").map(light_dark).unwrap_or_default();
        let mut times: Vec<(f64, usize)> = h
            .get("ti")
            .and_then(Plist::as_array)
            .map(|a| a.iter().filter_map(|e| Some((e.get("t")?.as_f64()?, e.get("i")?.as_index()?))).collect())
            .unwrap_or_default();
        times.retain(|(t, _)| (0.0..=1.0).contains(t));
        times.sort_by(|a, b| a.0.total_cmp(&b.0));
        return if times.is_empty() { Err("the picture's time-of-day metadata names no frames") } else { Ok(Schedule::TimeOfDay { light, dark, times }) };
    }
    if let Some(a) = apr {
        return match light_dark(a) {
            (Some(light), Some(dark)) => Ok(Schedule::Appearance { light, dark }),
            _ => Err("the picture's light and dark metadata is incomplete"),
        };
    }
    Ok(Schedule::Still)
}

/// The style picked for a dynamic picture in System Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Light or dark with the appearance.
    Automatic,
    /// By the time of day or the sun.
    Dynamic,
    /// Always the light frame.
    Light,
    /// Always the dark frame.
    Dark,
}

impl Style {
    /// From macOS's wallpaper settings (`style` picker id).
    pub fn from_store(id: &str) -> Option<Self> {
        match id.to_ascii_lowercase().as_str() {
            "automatic" | "auto" => Some(Style::Automatic),
            "dynamic" => Some(Style::Dynamic),
            "light" => Some(Style::Light),
            "dark" => Some(Style::Dark),
            _ => None,
        }
    }
}

/// The clock and place a frame is picked by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    /// Seconds since 1970 (UTC).
    pub unix: f64,
    /// The local time as a fraction of the day (0 at midnight).
    pub day_fraction: f64,
    /// Latitude and longitude (degrees, north and east positive) of the time zone's place.
    pub place: Option<(f64, f64)>,
}

/// The frame to draw, and when to look again (seconds; a time-of-day boundary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The image index in the file.
    pub index: usize,
    /// Seconds until the frame may change by the clock.
    pub recheck_in: Option<f64>,
}

/// How far (degrees of latitude and longitude) the real place may lie from the time zone's,
/// for a solar frame to be drawn: every place that far off must show the same frame.
pub const SOLAR_SLACK_DEG: f64 = 6.0;

/// How often a solar frame is picked again (seconds).
pub const SOLAR_RECHECK: f64 = 60.0;

/// The frame macOS shows of a picture with `schedule`, `count` frames and primary frame
/// `primary`, under `style` (from macOS's settings), the appearance (`dark`) and `clock`.
pub fn frame(schedule: &Schedule, style: Option<Style>, dark: bool, clock: &Clock, primary: usize, count: usize) -> Result<Frame, &'static str> {
    let still = |i: Option<usize>| i.map(|index| Frame { index, recheck_in: None }).ok_or("the picture's metadata has no light or dark frame");
    let by_look = |light: Option<usize>, d: Option<usize>| match style {
        Some(Style::Light) => still(light),
        Some(Style::Dark) => still(d),
        Some(Style::Automatic) => still(if dark { d } else { light }),
        _ => Err("the style picked for this dynamic picture is unknown"),
    };
    let f = match schedule {
        Schedule::Still => Ok(Frame { index: primary, recheck_in: None }),
        Schedule::Appearance { light, dark: d } => match style {
            Some(Style::Dynamic) => still(Some(if dark { *d } else { *light })),
            Some(_) => by_look(Some(*light), Some(*d)),
            None => Err("the style picked for this light and dark picture is unknown"),
        },
        Schedule::TimeOfDay { light, dark: d, times } => match style {
            Some(Style::Dynamic) => Ok(time_frame(times, clock.day_fraction)),
            _ => by_look(*light, *d),
        },
        Schedule::Solar { light, dark: d, suns } => match style {
            Some(Style::Dynamic) => {
                let place = clock.place.ok_or("a solar picture needs a place; the time zone has none")?;
                solar_frame(suns, clock.unix, place).map(|index| Frame { index, recheck_in: Some(SOLAR_RECHECK) })
            }
            _ => by_look(*light, *d),
        },
    }?;
    if f.index < count { Ok(f) } else { Err("the picture's metadata names a frame the file lacks") }
}

/// The time-of-day frame at `fraction` of the day: the last entry at or before it (the
/// day's last one before the first entry), and the seconds to the next entry.
pub fn time_frame(times: &[(f64, usize)], fraction: f64) -> Frame {
    let f = fraction.rem_euclid(1.0);
    let at = times.iter().rposition(|(t, _)| *t <= f).unwrap_or(times.len() - 1);
    let next = times.iter().map(|(t, _)| *t).find(|t| *t > f).unwrap_or(times[0].0 + 1.0);
    Frame { index: times[at].1, recheck_in: Some((next - f) * 86_400.0 + 1.0) }
}

/// The sun's altitude and azimuth (degrees; azimuth from north, clockwise) at `unix` seen
/// from `(lat, lon)`: the low-precision almanac formulas (about 0.01°, far below the
/// frames' spacing).
pub fn sun_position(unix: f64, lat: f64, lon: f64) -> (f64, f64) {
    let rad = f64::to_radians;
    let n = unix / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let l = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let g = rad((357.528 + 0.985_600_3 * n).rem_euclid(360.0));
    let lambda = rad(l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin());
    let eps = rad(23.439 - 0.000_000_4 * n);
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos());
    let dec = (eps.sin() * lambda.sin()).asin();
    let gmst = (18.697_374_558 + 24.065_709_824_419_08 * n).rem_euclid(24.0) * 15.0;
    let ha = rad(gmst + lon) - ra;
    let phi = rad(lat);
    let alt = (phi.sin() * dec.sin() + phi.cos() * dec.cos() * ha.cos()).asin();
    let az = (-dec.cos() * ha.sin()).atan2(dec.sin() * phi.cos() - dec.cos() * ha.cos() * phi.sin());
    (alt.to_degrees(), az.to_degrees().rem_euclid(360.0))
}

fn nearest(suns: &[SunFrame], alt: f64, az: f64) -> Option<usize> {
    let rad = f64::to_radians;
    let closeness = |s: &SunFrame| {
        rad(alt).sin() * rad(s.altitude).sin() + rad(alt).cos() * rad(s.altitude).cos() * rad(az - s.azimuth).cos()
    };
    suns.iter().max_by(|a, b| closeness(a).total_cmp(&closeness(b))).map(|s| s.index)
}

/// The solar frame at `unix` for a place near `place`: the frame whose sun lies nearest the
/// sun's place in the sky (great-circle distance), the same for every place within
/// [`SOLAR_SLACK_DEG`] of `place`; otherwise the frame cannot be known exactly.
pub fn solar_frame(suns: &[SunFrame], unix: f64, place: (f64, f64)) -> Result<usize, &'static str> {
    let mut picked = None;
    for dlat in [-SOLAR_SLACK_DEG, 0.0, SOLAR_SLACK_DEG] {
        for dlon in [-SOLAR_SLACK_DEG, 0.0, SOLAR_SLACK_DEG] {
            let (alt, az) = sun_position(unix, (place.0 + dlat).clamp(-89.0, 89.0), place.1 + dlon);
            let i = nearest(suns, alt, az).ok_or("the picture's solar metadata names no frames")?;
            if picked.is_some_and(|p| p != i) {
                return Err("the sun is near a frame change for the time zone's area; off until it passes");
            }
            picked = Some(i);
        }
    }
    picked.ok_or("the picture's solar metadata names no frames")
}

/// The place (latitude, longitude) of `zone` in a tz `zone.tab` file.
pub fn zone_place(tab: &str, zone: &str) -> Option<(f64, f64)> {
    tab.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
        let mut cols = l.split('\t');
        let (_, coords, name) = (cols.next()?, cols.next()?, cols.next()?);
        (name == zone).then(|| iso6709(coords)).flatten()
    })
}

/// `±DDMM±DDDMM` or `±DDMMSS±DDDMMSS` as degrees.
fn iso6709(s: &str) -> Option<(f64, f64)> {
    let split = s.char_indices().skip(1).find(|(_, c)| *c == '+' || *c == '-')?.0;
    let part = |p: &str, deg_digits: usize| -> Option<f64> {
        let (sign, digits) = p.split_at(1);
        let sign = if sign == "-" { -1.0 } else { 1.0 };
        if !digits.chars().all(|c| c.is_ascii_digit()) || digits.len() < deg_digits + 2 {
            return None;
        }
        let num = |a: usize, b: usize| digits.get(a..b).and_then(|d| d.parse::<f64>().ok());
        let deg = num(0, deg_digits)?;
        let min = num(deg_digits, deg_digits + 2)?;
        let sec = if digits.len() >= deg_digits + 4 { num(deg_digits + 2, deg_digits + 4)? } else { 0.0 };
        Some(sign * (deg + min / 60.0 + sec / 3600.0))
    };
    Some((part(&s[..split], 2)?, part(&s[split..], 3)?))
}

// ----- macOS's wallpaper settings ------------------------------------------------------

/// One Desktop wallpaper choice in macOS's wallpaper settings
/// (`~/Library/Application Support/com.apple.wallpaper/Store/Index.plist`, an undocumented
/// file read as data, never written): per display, per Space, or for all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoreChoice {
    /// The picture's path (from its file URL).
    pub path: String,
    /// The provider (`com.apple.wallpaper.choice.image` for a picture file).
    pub provider: String,
    /// The style picker's id (`automatic`, `dynamic`, `light`, `dark`).
    pub style: Option<String>,
    /// The placement picker's id (`Crop`, `Fit`, `Stretch`, `Center`, `Tile`).
    pub placement: Option<String>,
    /// The choice shuffles pictures.
    pub shuffle: bool,
}

/// The path of a `file://` URL (percent escapes decoded); `None` for another scheme.
pub fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8(out).ok()?;
    let trimmed = path.trim_end_matches('/');
    Some(if trimmed.is_empty() { "/".into() } else { trimmed.into() })
}

/// Every Desktop choice in the settings file's tree (each `Desktop` entry's `Content`).
pub fn store_choices(index: &Plist) -> Vec<StoreChoice> {
    fn walk(p: &Plist, out: &mut Vec<StoreChoice>, depth: usize) {
        if depth > 12 {
            return;
        }
        match p {
            Plist::Dict(entries) => {
                for (k, v) in entries {
                    if k == "Desktop"
                        && let Some(content) = v.get("Content")
                    {
                        read_content(content, out);
                    }
                    walk(v, out, depth + 1);
                }
            }
            Plist::Array(a) => a.iter().for_each(|v| walk(v, out, depth + 1)),
            _ => {}
        }
    }
    fn read_content(content: &Plist, out: &mut Vec<StoreChoice>) {
        let values = content.at(&["EncodedOptionValues", "values"]);
        let picked = |key: &str| values.and_then(|v| v.at(&[key, "picker", "_0", "id"])).and_then(Plist::as_str).map(str::to_owned);
        let shuffle = content.get("Shuffle").is_some_and(|s| !matches!(s, Plist::Str(s) if s == "$null") && !matches!(s, Plist::Other));
        for choice in content.get("Choices").and_then(Plist::as_array).unwrap_or_default() {
            let url = choice.at(&["Configuration", "url"]);
            let url = url.and_then(|u| u.as_str().or_else(|| u.get("relative").and_then(Plist::as_str)));
            let Some(path) = url.and_then(file_url_path) else { continue };
            out.push(StoreChoice {
                path,
                provider: choice.get("Provider").and_then(Plist::as_str).unwrap_or_default().to_owned(),
                style: picked("style"),
                placement: picked("placement"),
                shuffle,
            });
        }
    }
    let mut out = Vec::new();
    walk(index, &mut out, 0);
    out
}

/// What the settings say about the picture at `path`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StoreView {
    /// The style, when every choice of this picture names the same one.
    pub style: Option<Style>,
    /// The placement, when every choice of this picture names the same one.
    pub placement: Option<Placement>,
}

/// A provider whose picture is the file itself (a picture; a solid color's swatch).
fn drawable_provider(p: &str) -> bool {
    p.ends_with(".image") || p.to_ascii_lowercase().contains("color")
}

/// Consults the settings (`None`: unreadable or holding no choice at all) for the picture
/// at `path`: no choice naming it means macOS shows something else there (an aerial, a
/// shuffle, Photos); a shuffle or another provider is not drawn; Desktops with this picture
/// at different styles leave the style unknown.
pub fn consult(store: Option<&[StoreChoice]>, path: &str) -> Result<Option<StoreView>, &'static str> {
    let Some(store) = store.filter(|s| !s.is_empty()) else { return Ok(None) };
    let mine: Vec<&StoreChoice> = store.iter().filter(|c| c.path == path).collect();
    if mine.is_empty() {
        return Err("macOS's wallpaper settings do not name this picture (an aerial, a shuffle or Photos?)");
    }
    if mine.iter().any(|c| c.shuffle) {
        return Err("a shuffling wallpaper");
    }
    if mine.iter().any(|c| !drawable_provider(&c.provider)) {
        return Err("a wallpaper that is not a picture file");
    }
    let one = |vals: Vec<Option<&str>>| -> Result<Option<String>, ()> {
        let mut v: Vec<Option<String>> = vals.into_iter().map(|s| s.map(str::to_ascii_lowercase)).collect();
        v.dedup();
        match v.as_slice() {
            [single] => Ok(single.clone()),
            _ => Err(()),
        }
    };
    let style = match one(mine.iter().map(|c| c.style.as_deref()).collect()) {
        Ok(None) => None,
        Ok(Some(s)) => Some(Style::from_store(&s).ok_or("an unknown wallpaper style")?),
        Err(()) => return Err("Desktops showing this picture differ in light, dark or dynamic; which one shows is unknown"),
    };
    let placement = one(mine.iter().map(|c| c.placement.as_deref()).collect()).ok().flatten().and_then(|p| Placement::from_store(&p));
    Ok(Some(StoreView { style, placement }))
}

// ----- the decision ------------------------------------------------------------------------

/// The picture file's facts (ImageIO).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageFacts {
    /// Its size (the frames share it).
    pub size: ImageSize,
    /// How many frames.
    pub count: usize,
    /// The primary frame.
    pub primary: usize,
    /// Its schedule ([`schedule`]).
    pub schedule: Result<Schedule, &'static str>,
}

/// Everything [`decide`] reads.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    /// The picture NSWorkspace reports for the screen.
    pub path: Option<&'a str>,
    /// The path is a folder.
    pub is_dir: bool,
    /// NSWorkspace's placement ([`Placement::from_workspace`]).
    pub workspace: Option<Placement>,
    /// macOS's wallpaper settings (`None`: unreadable).
    pub store: Option<&'a [StoreChoice]>,
    /// The picture's facts (`None`: unreadable).
    pub image: Option<&'a ImageFacts>,
    /// NSWorkspace names a fill colour for the screen.
    pub fill: bool,
    /// The effective appearance is dark.
    pub dark: bool,
    /// The clock and place.
    pub clock: Clock,
}

/// What to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    /// How the picture lands.
    pub placement: Placement,
    /// Which frame.
    pub frame: Frame,
    /// The style macOS's settings name.
    pub style: Option<Style>,
    /// The picture's schedule kind ([`Schedule::kind`]).
    pub kind: &'static str,
}

/// Whether and how the picture can be drawn exactly as macOS shows it; the reason when not.
pub fn decide(i: &Inputs) -> Result<Decision, &'static str> {
    let path = i.path.ok_or("NSWorkspace reports no desktop picture")?;
    if i.is_dir {
        return Err("a folder of pictures (a shuffle)");
    }
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    if matches!(ext, "mov" | "mp4" | "m4v" | "avi" | "hevc") {
        return Err("a video wallpaper");
    }
    if ext == "madesktop" {
        return Err("a system wallpaper description, not a picture file");
    }
    if lower.contains("com.apple.idleassetsd") || lower.contains("/aerials/") {
        return Err("an aerial wallpaper");
    }
    let view = consult(i.store, path)?;
    let image = i.image.ok_or("the picture cannot be read")?;
    let schedule = image.schedule.as_ref().map_err(|e| *e)?;
    let style = view.and_then(|v| v.style);
    let placement = view.and_then(|v| v.placement).or(i.workspace).ok_or("the picture's scaling has no System Settings form")?;
    match placement {
        Placement::Center => return Err("Center places the picture by an unverified rule; off until a live check covers it"),
        Placement::Tile => return Err("Tile places the picture by an unverified rule; off until a live check covers it"),
        Placement::Fit if !i.fill => return Err("Fit with no fill colour: what macOS shows around the picture is unverified"),
        Placement::Fill | Placement::Fit | Placement::Stretch => {}
    }
    let frame = frame(schedule, style, i.dark, &i.clock, image.primary, image.count)?;
    Ok(Decision { placement, frame, style, kind: schedule.kind() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: Size = Size::new(1800.0, 1169.0);

    fn square() -> ImageSize {
        ImageSize::new(6016.0, 6016.0, Some(72.0), Some(72.0))
    }

    fn dict(entries: &[(&str, Plist)]) -> Plist {
        Plist::Dict(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    fn s(v: &str) -> Plist {
        Plist::Str(v.into())
    }

    fn n(v: f64) -> Plist {
        Plist::Num(v)
    }

    #[test]
    fn fill_covers_the_screen_centered_and_fit_shows_it_whole() {
        let area = Rect::new(0.0, 0.0, LAPTOP.w, LAPTOP.h);
        let fill = placed(Placement::Fill, LAPTOP, square(), area);
        assert_eq!(fill, vec![Rect::new(0.0, (1169.0 - 1800.0) / 2.0, 1800.0, 1800.0)]);
        let fit = placed(Placement::Fit, LAPTOP, square(), area);
        assert_eq!(fit, vec![Rect::new((1800.0 - 1169.0) / 2.0, 0.0, 1169.0, 1169.0)]);
        // A wide picture on the same screen: Fill matches the height, Fit the width.
        let wide = ImageSize::new(6000.0, 2000.0, None, None);
        let f = placed(Placement::Fill, LAPTOP, wide, area)[0];
        assert!((f.h - 1169.0).abs() < 1e-9 && (f.w - 3507.0).abs() < 1e-9 && (f.x + (3507.0 - 1800.0) / 2.0).abs() < 1e-9);
        let f = placed(Placement::Fit, LAPTOP, wide, area)[0];
        assert!((f.w - 1800.0).abs() < 1e-9 && (f.h - 600.0).abs() < 1e-9 && (f.y - 284.5).abs() < 1e-9);
        assert_eq!(placed(Placement::Stretch, LAPTOP, wide, area), vec![area]);
    }

    #[test]
    fn center_and_tile_use_the_pictures_own_size_in_points() {
        let area = Rect::new(1500.0, 800.0, 300.0, 369.0);
        // 1000 px at 144 dpi is 500 points.
        let img = ImageSize::new(1000.0, 600.0, Some(144.0), Some(144.0));
        assert_eq!(placed(Placement::Center, LAPTOP, img, area), vec![Rect::new(650.0, 434.5, 500.0, 300.0)]);
        let tiles = placed(Placement::Tile, LAPTOP, img, area);
        assert!(tiles.contains(&Rect::new(1500.0, 900.0, 500.0, 300.0)));
        assert!(tiles.contains(&Rect::new(1500.0, 600.0, 500.0, 300.0)));
        assert!(tiles.iter().all(|t| t.intersect(&area).is_some()), "{tiles:?}");
        assert_eq!(tiles.len(), 2);
    }

    #[test]
    fn a_draw_puts_the_whole_picture_where_the_screen_shows_it() {
        // His laptop: a 6016 px square, Fill on 1800x1169.
        let area = Rect::new(1700.0, 1000.0, 100.0, 169.0);
        let d = draws(Placement::Fill, LAPTOP, square(), area);
        let top = (1169.0 - 1800.0) / 2.0;
        assert_eq!(d, vec![Draw { dst: Rect::new(-1700.0, top - 1000.0, 1800.0, 1800.0) }]);
        assert!(d[0].dst.contains_rect(&Rect::new(0.0, 0.0, area.w, area.h)), "covers the area");
    }

    #[test]
    fn fit_leaves_the_corner_to_the_fill_color() {
        // A tall picture fitted on a wide screen leaves both sides empty.
        let tall = ImageSize::new(1000.0, 2000.0, None, None);
        let corner = Rect::new(1700.0, 1000.0, 100.0, 169.0);
        assert!(draws(Placement::Fit, LAPTOP, tall, corner).is_empty());
        assert!(!draws(Placement::Fit, LAPTOP, tall, Rect::new(800.0, 0.0, 100.0, 100.0)).is_empty());
    }

    #[test]
    fn the_region_holds_the_patch_and_crops_are_whole_pixels() {
        let patch = Rect::new(1739.0, 1043.0, 61.0, 126.0);
        let r = region(LAPTOP, patch);
        assert!(r.contains_rect(&patch));
        assert_eq!((r.max_x(), r.max_y()), (1800.0, 1169.0));
        assert_eq!((r.w, r.h), (BLOCK.w, BLOCK.h));
        let px = crop_px(r, patch, 2.0).expect("inside");
        assert_eq!(px, Rect::new((1739.0 - r.x) * 2.0, (1043.0 - r.y) * 2.0, 122.0, 252.0));
        // A patch bigger than the block grows the region; one outside it does not crop.
        let big = Rect::new(1000.0, 300.0, 800.0, 869.0);
        assert!(region(LAPTOP, big).contains_rect(&big));
        assert_eq!(crop_px(r, big, 2.0), None);
        // A small screen: the region stays on it.
        let small = Size::new(400.0, 300.0);
        assert_eq!(region(small, Rect::new(350.0, 250.0, 50.0, 50.0)), Rect::new(0.0, 0.0, 400.0, 300.0));
    }

    #[test]
    fn a_second_display_is_drawn_in_its_own_coordinates() {
        // A display right of the main one, its top 200 points lower.
        let display = Rect::new(1800.0, 200.0, 2560.0, 1440.0);
        let patch = Rect::new(4300.0, 1500.0, 60.0, 140.0);
        let l = local(patch, display);
        assert_eq!(l, Rect::new(2500.0, 1300.0, 60.0, 140.0));
        let screen = Size::new(display.w, display.h);
        let r = region(screen, l);
        assert_eq!((r.max_x(), r.max_y()), (2560.0, 1440.0));
        assert!(crop_px(r, l, 2.0).is_some());
    }

    #[test]
    fn placements_from_nsworkspace_and_from_the_settings() {
        assert_eq!(Placement::from_workspace(Some(3), Some(true)), Some(Placement::Fill));
        assert_eq!(Placement::from_workspace(Some(3), Some(false)), Some(Placement::Fit));
        assert_eq!(Placement::from_workspace(None, None), Some(Placement::Fit));
        assert_eq!(Placement::from_workspace(Some(1), Some(true)), Some(Placement::Stretch));
        assert_eq!(Placement::from_workspace(Some(2), None), Some(Placement::Center));
        assert_eq!(Placement::from_workspace(Some(0), Some(true)), None, "proportionally down: no System Settings form");
        assert_eq!(Placement::from_store("Crop"), Some(Placement::Fill));
        assert_eq!(Placement::from_store("Tile"), Some(Placement::Tile));
        assert_eq!(Placement::from_store("Mosaic"), None);
    }

    fn ventura_solar() -> Plist {
        let sun = |a: f64, z: f64, i: f64| dict(&[("a", n(a)), ("z", n(z)), ("i", n(i))]);
        dict(&[
            ("ap", dict(&[("d", n(1.0)), ("l", n(0.0))])),
            (
                "si",
                Plist::Array(vec![
                    sun(25.0, 110.0, 0.0),
                    sun(-25.0, 70.0, 1.0),
                    sun(-9.0, 80.0, 2.0),
                    sun(0.0, 90.0, 3.0),
                    sun(10.0, 100.0, 4.0),
                    sun(25.0, 250.0, 0.0),
                    sun(10.0, 260.0, 4.0),
                    sun(0.0, 270.0, 3.0),
                    sun(-9.0, 280.0, 2.0),
                    sun(-25.0, 290.0, 1.0),
                ]),
            ),
        ])
    }

    fn clock(unix: f64, frac: f64) -> Clock {
        Clock { unix, day_fraction: frac, place: Some((34.05, -118.24)) }
    }

    #[test]
    fn the_owners_wallpaper_set_to_dark_shows_its_dark_frame_always() {
        let sched = schedule(None, None, Some(&ventura_solar())).expect("solar");
        assert_eq!(sched.kind(), "solar");
        for (unix, dark) in [(1_760_000_000.0, false), (1_760_040_000.0, true)] {
            let f = frame(&sched, Some(Style::Dark), dark, &clock(unix, 0.5), 0, 5).expect("a frame");
            assert_eq!(f, Frame { index: 1, recheck_in: None });
        }
        assert_eq!(frame(&sched, Some(Style::Light), true, &clock(0.0, 0.5), 0, 5).map(|f| f.index), Ok(0));
        assert_eq!(frame(&sched, Some(Style::Automatic), true, &clock(0.0, 0.5), 0, 5).map(|f| f.index), Ok(1));
        assert_eq!(frame(&sched, Some(Style::Automatic), false, &clock(0.0, 0.5), 0, 5).map(|f| f.index), Ok(0));
        assert!(frame(&sched, None, true, &clock(0.0, 0.5), 0, 5).is_err(), "style unknown: not drawn");
        assert!(frame(&sched, Some(Style::Dark), true, &clock(0.0, 0.5), 0, 1).is_err(), "frame missing from the file");
    }

    #[test]
    fn the_sun_is_where_the_almanac_puts_it() {
        // 2026-06-21 20:00 UTC, Los Angeles (noon PDT is 19:00 UTC; solar noon ~19:56).
        let unix = 1_782_072_000.0;
        let (alt, az) = sun_position(unix, 34.05, -118.24);
        assert!((alt - 79.4).abs() < 1.0, "{alt}");
        assert!((az - 180.0).abs() < 15.0, "{az}");
        // Midnight there: well below the horizon.
        let (alt, _) = sun_position(unix + 12.0 * 3600.0, 34.05, -118.24);
        assert!(alt < -30.0, "{alt}");
        // Morning: east; evening: west.
        let (_, az) = sun_position(unix - 5.0 * 3600.0, 34.05, -118.24);
        assert!((45.0..135.0).contains(&az), "{az}");
        let (_, az) = sun_position(unix + 5.0 * 3600.0, 34.05, -118.24);
        assert!((225.0..315.0).contains(&az), "{az}");
    }

    #[test]
    fn solar_frames_show_only_when_the_whole_time_zone_agrees() {
        let Schedule::Solar { suns, .. } = schedule(None, None, Some(&ventura_solar())).expect("solar") else { panic!() };
        let la = (34.05, -118.24);
        let noon = 1_782_072_000.0;
        assert_eq!(solar_frame(&suns, noon, la), Ok(0), "high sun: the day frame");
        assert_eq!(solar_frame(&suns, noon + 12.0 * 3600.0, la), Ok(1), "midnight: the night frame");
        // Somewhere around sunset (~03:08 UTC), the frames change within the slack: off.
        let mut off = 0;
        for minute in 0..240 {
            if solar_frame(&suns, noon + 5.0 * 3600.0 + minute as f64 * 60.0, la).is_err() {
                off += 1;
            }
        }
        assert!(off > 0 && off < 240, "{off}");
        let sched = Schedule::Solar { light: Some(0), dark: Some(1), suns };
        let f = frame(&sched, Some(Style::Dynamic), false, &clock(noon, 0.5), 0, 5).expect("noon");
        assert_eq!(f, Frame { index: 0, recheck_in: Some(SOLAR_RECHECK) });
        let nowhere = Clock { place: None, ..clock(noon, 0.5) };
        assert!(frame(&sched, Some(Style::Dynamic), false, &nowhere, 0, 5).is_err());
    }

    #[test]
    fn time_of_day_frames_follow_the_clock_and_wrap_at_midnight() {
        let entry = |t: f64, i: f64| dict(&[("t", n(t)), ("i", n(i))]);
        let h24 = dict(&[
            ("ap", dict(&[("d", n(3.0)), ("l", n(1.0))])),
            ("ti", Plist::Array(vec![entry(0.75, 3.0), entry(0.25, 1.0), entry(0.5, 2.0)])),
        ]);
        let sched = schedule(None, Some(&h24), None).expect("h24");
        assert_eq!(sched.kind(), "time of day");
        let at = |frac: f64| frame(&sched, Some(Style::Dynamic), false, &clock(0.0, frac), 0, 4).expect("frame");
        assert_eq!(at(0.3).index, 1);
        assert_eq!(at(0.5).index, 2);
        assert_eq!(at(0.9).index, 3);
        assert_eq!(at(0.1).index, 3, "before the first entry: last night's");
        assert!((at(0.3).recheck_in.expect("boundary") - (0.2 * 86_400.0 + 1.0)).abs() < 1e-6);
        assert!((at(0.9).recheck_in.expect("boundary") - (0.35 * 86_400.0 + 1.0)).abs() < 1e-6, "to tomorrow's first");
        assert_eq!(frame(&sched, Some(Style::Light), true, &clock(0.0, 0.9), 0, 4).map(|f| f.index), Ok(1));
    }

    #[test]
    fn light_and_dark_pictures_follow_the_appearance_unless_pinned() {
        let apr = dict(&[("d", n(1.0)), ("l", n(0.0))]);
        let sched = schedule(Some(&apr), None, None).expect("apr");
        let c = clock(0.0, 0.5);
        assert_eq!(frame(&sched, Some(Style::Automatic), true, &c, 0, 2).map(|f| f.index), Ok(1));
        assert_eq!(frame(&sched, Some(Style::Automatic), false, &c, 0, 2).map(|f| f.index), Ok(0));
        assert_eq!(frame(&sched, Some(Style::Light), true, &c, 0, 2).map(|f| f.index), Ok(0));
        assert!(frame(&sched, None, true, &c, 0, 2).is_err());
        // A still: the primary frame whatever the style.
        assert_eq!(frame(&Schedule::Still, None, true, &c, 2, 3).map(|f| f.index), Ok(2));
    }

    #[test]
    fn base64_and_numeric_strings_decode() {
        assert_eq!(base64_decode("YnBsaXN0MDA=").as_deref(), Some(&b"bplist00"[..]));
        assert_eq!(base64_decode("TWE=").as_deref(), Some(&b"Ma"[..]));
        assert_eq!(base64_decode("TWFu").as_deref(), Some(&b"Man"[..]));
        assert_eq!(base64_decode("T?Fu"), None);
        assert_eq!(s("-25").as_f64(), Some(-25.0));
        assert_eq!(n(2.0).as_index(), Some(2));
        assert_eq!(n(-1.0).as_index(), None);
    }

    #[test]
    fn time_zone_places_come_from_zone_tab() {
        let tab = "# comment\nUS\t+340308-1181434\tAmerica/Los_Angeles\tPacific\nUA\t+5026+03031\tEurope/Kyiv\tmost of Ukraine\n";
        let (lat, lon) = zone_place(tab, "America/Los_Angeles").expect("LA");
        assert!((lat - 34.052).abs() < 0.001 && (lon + 118.243).abs() < 0.001, "{lat} {lon}");
        let (lat, lon) = zone_place(tab, "Europe/Kyiv").expect("Kyiv");
        assert!((lat - 50.433).abs() < 0.001 && (lon - 30.517).abs() < 0.001);
        assert_eq!(zone_place(tab, "Mars/Olympus"), None);
    }

    /// A choice: its URL, provider, style, placement and whether it shuffles.
    type Row<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>, bool);

    fn store_with(choices: &[Row]) -> Plist {
        let content = |url: &str, provider: &str, style: Option<&str>, placement: Option<&str>, shuffle: bool| {
            let picker = |id: &str| dict(&[("picker", dict(&[("_0", dict(&[("id", s(id))]))]))]);
            let mut values = Vec::new();
            if let Some(st) = style {
                values.push(("style", picker(st)));
            }
            if let Some(p) = placement {
                values.push(("placement", picker(p)));
            }
            dict(&[
                (
                    "Choices",
                    Plist::Array(vec![dict(&[
                        ("Configuration", dict(&[("type", s("imageFile")), ("url", dict(&[("relative", s(url))]))])),
                        ("Provider", s(provider)),
                    ])]),
                ),
                ("EncodedOptionValues", dict(&[("values", dict(&values))])),
                ("Shuffle", s(if shuffle { "{folder}" } else { "$null" })),
            ])
        };
        let spaces: Vec<(String, Plist)> = choices
            .iter()
            .enumerate()
            .map(|(i, (u, p, st, pl, sh))| (format!("space-{i}"), dict(&[("Default", dict(&[("Desktop", dict(&[("Content", content(u, p, *st, *pl, *sh))]))]))])))
            .collect();
        dict(&[("Spaces", Plist::Dict(spaces)), ("AllSpacesAndDisplays", dict(&[("Idle", dict(&[]))]))])
    }

    const HEIC: &str = "file:///Users/someone/Library/Application%20Support/com.apple.mobileAssetDesktop/Ventura%20Graphic.heic";
    const HEIC_PATH: &str = "/Users/someone/Library/Application Support/com.apple.mobileAssetDesktop/Ventura Graphic.heic";
    const IMAGE: &str = "com.apple.wallpaper.choice.image";

    #[test]
    fn the_settings_name_the_style_and_placement_of_the_picture_shown() {
        let store = store_choices(&store_with(&[(HEIC, IMAGE, Some("dark"), Some("Crop"), false); 3]));
        assert_eq!(store.len(), 3);
        assert_eq!(store[0].path, HEIC_PATH);
        let view = consult(Some(&store), HEIC_PATH).expect("named").expect("read");
        assert_eq!(view, StoreView { style: Some(Style::Dark), placement: Some(Placement::Fill) });
        assert!(consult(Some(&store), "/elsewhere.jpg").is_err(), "not named: not drawn");
        assert_eq!(consult(None, HEIC_PATH), Ok(None), "unreadable: NSWorkspace alone");
        assert_eq!(consult(Some(&[]), HEIC_PATH), Ok(None), "empty: as unreadable");
    }

    #[test]
    fn shuffles_other_providers_and_mixed_styles_stay_off() {
        let shuffled = store_choices(&store_with(&[(HEIC, IMAGE, Some("dark"), None, true)]));
        assert!(consult(Some(&shuffled), HEIC_PATH).is_err());
        let aerial = store_choices(&store_with(&[(HEIC, "com.apple.wallpaper.choice.aerials", None, None, false)]));
        assert!(consult(Some(&aerial), HEIC_PATH).is_err());
        let mixed = store_choices(&store_with(&[(HEIC, IMAGE, Some("dark"), None, false), (HEIC, IMAGE, Some("light"), None, false)]));
        assert!(consult(Some(&mixed), HEIC_PATH).is_err());
        // Mixed placements: NSWorkspace's, which is the shown Space's.
        let places = store_choices(&store_with(&[(HEIC, IMAGE, Some("dark"), Some("Crop"), false), (HEIC, IMAGE, Some("dark"), Some("Fit"), false)]));
        assert_eq!(consult(Some(&places), HEIC_PATH), Ok(Some(StoreView { style: Some(Style::Dark), placement: None })));
    }

    fn facts(schedule: Result<Schedule, &'static str>, count: usize) -> ImageFacts {
        ImageFacts { size: square(), count, primary: 0, schedule }
    }

    fn inputs<'a>(path: &'a str, store: Option<&'a [StoreChoice]>, image: Option<&'a ImageFacts>) -> Inputs<'a> {
        Inputs { path: Some(path), is_dir: false, workspace: Some(Placement::Fill), store, image, fill: true, dark: true, clock: clock(0.0, 0.5) }
    }

    #[test]
    fn the_decision_draws_the_owners_setup_and_refuses_what_it_cannot_match() {
        let store = store_choices(&store_with(&[(HEIC, IMAGE, Some("dark"), Some("Crop"), false)]));
        let solar = facts(schedule(None, None, Some(&ventura_solar())), 5);
        let d = decide(&inputs(HEIC_PATH, Some(&store), Some(&solar))).expect("drawn");
        assert_eq!((d.placement, d.frame.index, d.style, d.kind), (Placement::Fill, 1, Some(Style::Dark), "solar"));
        // The same picture with the settings unreadable: its style is unknown.
        assert!(decide(&inputs(HEIC_PATH, None, Some(&solar))).is_err());
        // A plain still with the settings unreadable: NSWorkspace's word suffices.
        let still = facts(Ok(Schedule::Still), 1);
        let d = decide(&inputs("/Pictures/beach.jpg", None, Some(&still))).expect("a still");
        assert_eq!((d.placement, d.frame.index), (Placement::Fill, 0));
        for (path, why) in [
            ("/Library/Application Support/com.apple.idleassetsd/Customer/4KSDR240FPS/x.mov", "video"),
            ("/System/Library/Desktop Pictures/Ventura Graphic.madesktop", "description"),
            ("/Library/Application Support/com.apple.idleassetsd/snapshots/x.png", "aerial"),
        ] {
            assert!(decide(&inputs(path, None, Some(&still))).is_err(), "{why}");
        }
        let folder = Inputs { is_dir: true, ..inputs("/Pictures/Holiday", None, Some(&still)) };
        assert!(decide(&folder).is_err());
        assert!(decide(&Inputs { path: None, ..inputs("", None, Some(&still)) }).is_err());
        assert!(decide(&inputs("/Pictures/beach.jpg", None, None)).is_err(), "unreadable picture");
        let no_scaling = Inputs { workspace: None, ..inputs("/Pictures/beach.jpg", None, Some(&still)) };
        assert!(decide(&no_scaling).is_err());
    }

    #[test]
    fn exact_or_off_center_tile_and_a_fit_with_no_fill_colour_stay_off() {
        let still = facts(Ok(Schedule::Still), 1);
        let beach = "/Pictures/beach.jpg";
        let with = |workspace: Option<Placement>, fill: bool| Inputs { workspace, fill, ..inputs(beach, None, Some(&still)) };
        // The settings unreadable: NSWorkspace's Fill, Fit and Stretch only.
        for p in [Placement::Fill, Placement::Fit, Placement::Stretch] {
            assert_eq!(decide(&with(Some(p), true)).map(|d| d.placement), Ok(p));
        }
        assert!(decide(&with(Some(Placement::Center), true)).is_err());
        // The settings naming Center or Tile.
        for id in ["Center", "Tile"] {
            let store = store_choices(&store_with(&[("file:///Pictures/beach.jpg", IMAGE, None, Some(id), false)]));
            let i = Inputs { store: Some(&store), ..with(Some(Placement::Fill), true) };
            let why = decide(&i).expect_err(id);
            assert!(why.contains(id) && why.contains("unverified"), "{why}");
        }
        // Fit with no fill colour; Fill and Stretch need none.
        assert!(decide(&with(Some(Placement::Fit), false)).is_err());
        assert!(decide(&with(Some(Placement::Fill), false)).is_ok());
        assert!(decide(&with(Some(Placement::Stretch), false)).is_ok());
    }

    #[test]
    fn a_draw_covers_the_area_or_the_background_shows() {
        let size = Size::new(480.0, 560.0);
        let fill = draws(Placement::Fill, LAPTOP, square(), Rect::new(1320.0, 609.0, size.w, size.h));
        assert!(covered(&fill, size));
        let tall = ImageSize::new(1000.0, 2000.0, None, None);
        let fit = draws(Placement::Fit, LAPTOP, tall, Rect::new(1000.0, 609.0, size.w, size.h));
        assert!(!fit.is_empty() && !covered(&fit, size), "a gap beside the fitted picture");
        assert!(!covered(&[], size));
    }
}
