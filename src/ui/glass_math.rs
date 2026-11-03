//! The cards' glass, as numbers (pure; no AppKit): how a window's picture on a card lets the
//! wallpaper through, as Apple's strip does (the owner's screenshots, 11-03).
//!
//! **The model.** A card shows each window's picture at `[cards] window_opacity` (`a`) over a
//! flat colour of its own, the *under*: the mean of the wallpaper behind the thumbnail, its
//! saturation scaled by `backdrop_saturation` (`s`, about its Rec. 709 luma) and its level by
//! `backdrop_brightness` (`k`):
//!
//! ```text
//! card  = a · picture + (1 − a) · under
//! under = k · (s · mean + (1 − s) · luma(mean))
//! ```
//!
//! all in the display's encoded values (Core Animation composites them so). Fitted to three
//! of Apple's cards over an orange wallpaper (Safari's empty page, Mail's and Fork's white),
//! `a = 0.70, s = 0.5, k = 1.0` misses by 5 levels on average, within what sampling the
//! wallpaper beside a card rather than under it explains. Other readings fit worse or say
//! nothing: plain alpha over the wallpaper as it is (12 levels: Fork's card is far greyer than
//! the dark wallpaper around it), a multiply (12), alpha over a darkened wallpaper (6.7, Fork
//! 13 off in red); a flat grey tint over the wallpaper fits as well as the model for any `a`
//! from 0.3 to 0.75, so white pages alone cannot tell `a` from a tint: black, grey and white
//! fixtures can (`scripts/measure-apple-card-glass.py`). The under is flat over a card: Apple's
//! Safari card read alike 100 pt apart where the wallpaper beside it changed by 26 levels, so
//! it is a mean over the thumbnail, not a local blur (a blur keeps a ramp a ramp).
//!
//! **The backdrop** ([`Backdrop`]): the wallpaper as a small grid of mean colours (a summed
//! table), so a thumbnail's mean is four lookups however large it is.

use crate::engine::geometry::Rect;

/// Rec. 709 luma weights (the saturation's pivot, as Core Image's colour controls).
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// `[cards] window_opacity`, `backdrop_saturation` and `backdrop_brightness`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glass {
    /// A window's picture on a card (1: opaque, the glass off).
    pub opacity: f64,
    /// The under's saturation, of the wallpaper's (0 grey, 1 as it is).
    pub saturation: f64,
    /// The under's level, of the wallpaper's.
    pub brightness: f64,
}

impl Glass {
    /// Apple's, as fitted (module notes).
    pub const APPLE: Glass = Glass { opacity: 0.7, saturation: 0.5, brightness: 1.0 };

    /// True when the cards let anything through.
    pub fn on(&self) -> bool {
        self.opacity < 1.0
    }

    /// The under for a mean wallpaper colour `mean` (0–1 a channel).
    pub fn under(&self, mean: [f64; 3]) -> [f64; 3] {
        let y = luma(mean);
        mean.map(|c| (self.brightness * (self.saturation * c + (1.0 - self.saturation) * y)).clamp(0.0, 1.0))
    }

    /// What a card shows where the picture is `page` (opaque) over `under`.
    pub fn card(&self, page: [f64; 3], under: [f64; 3]) -> [f64; 3] {
        let a = self.opacity.clamp(0.0, 1.0);
        [0, 1, 2].map(|i| a * page[i] + (1.0 - a) * under[i])
    }
}

/// The luma of an encoded colour.
pub fn luma(c: [f64; 3]) -> f64 {
    LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2]
}

/// A flying snapshot `amount` of the way to its card's look (0 the window, 1 the card): the
/// picture's opacity and the under's, for a card's picture opacity `card_opacity`. At 1 the
/// pair draws the card exactly (the under opaque, the picture at the card's opacity); at 0 the
/// window, the under gone.
pub fn ghost_mix(amount: f64, card_opacity: f64) -> (f64, f64) {
    let t = amount.clamp(0.0, 1.0);
    (1.0 - t * (1.0 - card_opacity.clamp(0.0, 1.0)), t)
}

/// What a card of picture opacity `a` shows over a black page and a white one say about it:
/// `(a, under)`, from the two colours read (0–1). `None` when the two are no blend (white not
/// above black).
pub fn solve(black: [f64; 3], white: [f64; 3]) -> Option<(f64, [f64; 3])> {
    let a = [0, 1, 2].map(|i| white[i] - black[i]).iter().sum::<f64>() / 3.0;
    if !(a > 0.0 && a < 1.0) {
        return None;
    }
    Some((a, black.map(|b| (b / (1.0 - a)).clamp(0.0, 1.0))))
}

/// The wallpaper as a grid of colours with a summed table over it (0–1 a channel, the
/// display's encoded values), covering `screen` (global, top-left).
#[derive(Debug, Clone, PartialEq)]
pub struct Backdrop {
    /// The screen the grid covers.
    pub screen: Rect,
    cols: usize,
    rows: usize,
    /// `(cols + 1) × (rows + 1)` running sums, row-major, a zero row and column first.
    sums: Vec<[f64; 3]>,
}

impl Backdrop {
    /// A backdrop from BGRA bytes (`w × h`, `stride` bytes a row, top row first, opaque) of a
    /// picture of `screen`. `None` when the bytes are short or there are none.
    pub fn from_bgra(bytes: &[u8], w: usize, h: usize, stride: usize, screen: Rect) -> Option<Self> {
        if w == 0 || h == 0 || stride < w * 4 || bytes.len() < stride * (h - 1) + w * 4 {
            return None;
        }
        let rgb = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| {
            let p = y * stride + x * 4;
            [bytes[p + 2], bytes[p + 1], bytes[p]].map(|v| f64::from(v) / 255.0)
        });
        Self::from_rgb(&rgb.collect::<Vec<_>>(), w, h, screen)
    }

    /// A backdrop from `w × h` colours (top row first).
    pub fn from_rgb(rgb: &[[f64; 3]], w: usize, h: usize, screen: Rect) -> Option<Self> {
        if w == 0 || h == 0 || rgb.len() < w * h || !(screen.w > 0.0 && screen.h > 0.0) {
            return None;
        }
        let stride = w + 1;
        let mut sums = vec![[0.0; 3]; stride * (h + 1)];
        for y in 0..h {
            let mut row = [0.0; 3];
            for x in 0..w {
                let c = rgb[y * w + x];
                for i in 0..3 {
                    row[i] += c[i];
                }
                let above = sums[y * stride + x + 1];
                sums[(y + 1) * stride + x + 1] = [0, 1, 2].map(|i| above[i] + row[i]);
            }
        }
        Some(Self { screen, cols: w, rows: h, sums })
    }

    /// The mean colour behind `rect` (global, top-left), clipped to the screen; `None` when it
    /// lies off the screen.
    pub fn mean(&self, rect: Rect) -> Option<[f64; 3]> {
        let s = self.screen;
        let (cw, ch) = (s.w / self.cols as f64, s.h / self.rows as f64);
        let x0 = ((rect.x - s.x) / cw).floor().max(0.0) as usize;
        let y0 = ((rect.y - s.y) / ch).floor().max(0.0) as usize;
        let x1 = ((rect.max_x() - s.x) / cw).ceil().min(self.cols as f64);
        let y1 = ((rect.max_y() - s.y) / ch).ceil().min(self.rows as f64);
        if !(x1 >= 1.0 && y1 >= 1.0) {
            return None;
        }
        let (x1, y1) = (x1 as usize, y1 as usize);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let stride = self.cols + 1;
        let at = |x: usize, y: usize| self.sums[y * stride + x];
        let (a, b, c, d) = (at(x1, y1), at(x0, y1), at(x1, y0), at(x0, y0));
        let n = ((x1 - x0) * (y1 - y0)) as f64;
        Some([0, 1, 2].map(|i| (a[i] - b[i] - c[i] + d[i]) / n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(c: [f64; 3]) -> [f64; 3] {
        c.map(|v| (v * 255.0).round())
    }

    fn rgb(r: f64, g: f64, b: f64) -> [f64; 3] {
        [r / 255.0, g / 255.0, b / 255.0]
    }

    #[test]
    fn apples_cards_over_an_orange_wallpaper_come_out_within_a_few_levels() {
        // (page, the card on Apple's strip, the wallpaper beside it), the owner's screenshots 11-03.
        let cases = [
            ("Safari", rgb(240.0, 240.0, 240.0), rgb(223.0, 198.0, 190.0), rgb(224.0, 70.0, 0.5)),
            ("Mail", rgb(255.0, 255.0, 255.0), rgb(231.0, 207.0, 194.0), rgb(244.0, 93.0, 29.0)),
            ("Fork", rgb(255.0, 255.0, 255.0), rgb(201.0, 190.0, 188.0), rgb(138.0, 31.0, 6.0)),
        ];
        let g = Glass::APPLE;
        let mut sq = 0.0;
        for (name, page, apple, wall) in cases {
            let ours = levels(g.card(page, g.under(wall)));
            let want = levels(apple);
            for i in 0..3 {
                let d = ours[i] - want[i];
                assert!(d.abs() <= 10.0, "{name}: {ours:?} vs Apple's {want:?}");
                sq += d * d;
            }
        }
        let rms = (sq / 9.0f64).sqrt();
        assert!(rms < 5.5, "{rms}");
    }

    #[test]
    fn the_under_keeps_the_luma_and_scales_the_colour_about_it() {
        let g = Glass { opacity: 0.7, saturation: 0.5, brightness: 1.0 };
        let wall = rgb(244.0, 93.0, 29.0);
        let u = g.under(wall);
        assert!((luma(u) - luma(wall)).abs() < 1e-9);
        assert!(u[0] < wall[0] && u[2] > wall[2], "toward grey: {u:?}");
        let grey = Glass { saturation: 0.0, ..g }.under(wall);
        assert!((grey[0] - grey[1]).abs() < 1e-12 && (grey[1] - grey[2]).abs() < 1e-12);
        assert_eq!(Glass { saturation: 1.0, ..g }.under(wall), wall);
        let dark = Glass { saturation: 1.0, brightness: 0.5, ..g }.under(wall);
        assert!((dark[0] - wall[0] / 2.0).abs() < 1e-12);
        assert_eq!(Glass { saturation: 1.0, brightness: 2.0, ..g }.under([0.8; 3]), [1.0; 3], "clamped");
    }

    #[test]
    fn opacity_one_is_the_opaque_card_and_the_glass_off() {
        let g = Glass { opacity: 1.0, ..Glass::APPLE };
        assert!(!g.on() && Glass::APPLE.on());
        assert_eq!(g.card(rgb(10.0, 20.0, 30.0), [1.0; 3]), rgb(10.0, 20.0, 30.0));
    }

    #[test]
    fn a_ghost_lands_on_the_cards_look_and_leaves_it_as_the_window() {
        assert_eq!(ghost_mix(0.0, 0.7), (1.0, 0.0), "the window: opaque, no under");
        let (picture, under) = ghost_mix(1.0, 0.7);
        assert!((picture - 0.7).abs() < 1e-12 && under == 1.0, "the card");
        // A spring past its end draws no more than the card, nor less than the window.
        assert_eq!(ghost_mix(1.2, 0.7), ghost_mix(1.0, 0.7));
        assert_eq!(ghost_mix(-0.1, 0.7), ghost_mix(0.0, 0.7));
        // The picture at `p` over the under at `u` (opaque colour) composites, at the end, to
        // the card's blend.
        let (p, u) = ghost_mix(1.0, 0.7);
        let (page, wall) = (0.9, 0.3);
        let drawn = p * page + (1.0 - p) * (u * wall);
        assert!((drawn - Glass::APPLE.card([page; 3], [wall; 3])[0]).abs() < 1e-12);
    }

    #[test]
    fn black_and_white_pages_give_the_opacity_and_the_under() {
        let g = Glass::APPLE;
        let under = rgb(180.0, 100.0, 70.0);
        let (black, white) = (g.card([0.0; 3], under), g.card([1.0; 3], under));
        let (a, u) = solve(black, white).expect("a blend");
        assert!((a - 0.7).abs() < 1e-9);
        assert!(u.iter().zip(under).all(|(x, y)| (x - y).abs() < 1e-9));
        assert_eq!(solve([0.5; 3], [0.5; 3]), None, "opaque: the page alone");
    }

    #[test]
    fn a_thumbnails_mean_is_the_wallpaper_behind_it() {
        // 4 x 2 cells over a 400 x 200 screen: the left half red, the right half blue.
        let red = [1.0, 0.0, 0.0];
        let blue = [0.0, 0.0, 1.0];
        let cells: Vec<[f64; 3]> = (0..8).map(|i| if i % 4 < 2 { red } else { blue }).collect();
        let b = Backdrop::from_rgb(&cells, 4, 2, Rect::new(0.0, 0.0, 400.0, 200.0)).expect("a backdrop");
        assert_eq!(b.mean(Rect::new(0.0, 0.0, 200.0, 200.0)), Some(red));
        assert_eq!(b.mean(Rect::new(200.0, 50.0, 100.0, 100.0)), Some(blue));
        assert_eq!(b.mean(Rect::new(100.0, 0.0, 200.0, 200.0)), Some([0.5, 0.0, 0.5]), "half each");
        assert_eq!(b.mean(Rect::new(-50.0, -50.0, 100.0, 100.0)), Some(red), "clipped to the screen");
        assert_eq!(b.mean(Rect::new(-300.0, 0.0, 100.0, 100.0)), None, "off the screen");
        assert_eq!(b.mean(Rect::new(500.0, 0.0, 100.0, 100.0)), None);
    }

    #[test]
    fn a_backdrop_reads_bgra_rows_top_first() {
        // 2 x 1 pixels, padded rows: BGRA blue then red.
        let bytes = [255, 0, 0, 255, 0, 0, 255, 255, 9, 9];
        let b = Backdrop::from_bgra(&bytes, 2, 1, 10, Rect::new(0.0, 0.0, 20.0, 10.0)).expect("a backdrop");
        assert_eq!(b.mean(Rect::new(0.0, 0.0, 10.0, 10.0)), Some([0.0, 0.0, 1.0]));
        assert_eq!(b.mean(Rect::new(10.0, 0.0, 10.0, 10.0)), Some([1.0, 0.0, 0.0]));
        assert_eq!(Backdrop::from_bgra(&bytes[..6], 2, 1, 8, Rect::new(0.0, 0.0, 20.0, 10.0)), None, "short");
    }
}
