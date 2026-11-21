//! The park corner covered through a switch ([`crate::engine::corner::corner_patch::switch_cover`],
//! round 8L): the arriving windows wait in the corner behind their pictures, where macOS has
//! raised an activated app's windows above the corner patch, so the switch's overlay draws
//! the desktop over the corner (the patch's picture, or a still of the desktop).

use crate::platform::clock;
use std::collections::HashSet;

use super::super::*;
use super::patch::parked_in_zone;

/// Switch cover `c` as it can be drawn now (round 8L3), or why not: where nothing lies on the
/// desktop, the corner patch's picture (`wallpaper`: ready for that rectangle) or, when that
/// is not ready (an aerial, a picture still rendering), the desktop's still; where a desktop
/// icon lies or the icons are not known, only the desktop's still (`desktop`: ready for that
/// rectangle), since the patch's picture would hide the icon. The reason given is the
/// picture's that was wanted first.
pub(in crate::platform::controller) fn resolve_cover(
    c: SwitchCover,
    wallpaper: impl FnOnce(Rect) -> Result<(), &'static str>,
    desktop: impl FnOnce(Rect) -> Result<(), &'static str>,
) -> Result<SwitchCover, &'static str> {
    let rect = c.rect;
    let picture = match c.picture {
        CoverPicture::Wallpaper => match wallpaper(rect) {
            Ok(()) => CoverPicture::Wallpaper,
            Err(why) => desktop(rect).map(|()| CoverPicture::Desktop).map_err(|_| why)?,
        },
        CoverPicture::Desktop => desktop(rect).map(|()| CoverPicture::Desktop)?,
    };
    Ok(SwitchCover { picture, ..c })
}

/// The bounds, as listed in `list`, of a switch's `arriving` windows macOS has raised above the
/// corner patch, which its cover must hide ([`crate::engine::corner::corner_patch::CoverFacts::raised`]):
/// those waiting in the park zone (`in_zone`), and the `activated` app's wherever macOS put
/// them (one brought into view, outside the corner's square, means no cover: round 8L6).
/// Normal, drawn, not ours (`own`).
pub(in crate::platform::controller) fn raised_windows(list: &[windows::CgWindow], own: i32, arriving: &HashSet<u32>, activated: &HashSet<u32>, in_zone: impl Fn(Rect) -> bool) -> Vec<Rect> {
    list.iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.pid != own && arriving.contains(&w.id))
        .filter(|w| in_zone(w.bounds) || activated.contains(&w.id))
        .map(|w| w.bounds)
        .collect()
}

/// The windows of window server list `list` (front to back) that stay shown through a switch
/// ([`Staying`]): normal, drawn, not ours (`own`), neither leaving (`outgoing`, the switch's
/// parks: the outgoing stage's windows never count, however much of the screen they fill),
/// nor arriving, nor parked in the park zone (`parked`, [`parked_in_zone`]): pinned windows,
/// unmanaged ones (another app's, an ignored app's), a dialog of one, and one the model has
/// parked that the window server lists elsewhere (8L6). `snapshot`: whether the strip holds a
/// full-size snapshot of a window, for the cover to draw it (round 8L5).
pub(in crate::platform::controller) fn staying_windows(
    list: &[windows::CgWindow],
    own: i32,
    outgoing: &HashSet<u32>,
    arriving: &HashSet<u32>,
    parked: &HashSet<u32>,
    snapshot: impl Fn(u32) -> bool,
) -> Vec<Staying> {
    list.iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.pid != own)
        .filter(|w| !outgoing.contains(&w.id) && !arriving.contains(&w.id) && !parked.contains(&w.id))
        .map(|w| Staying { id: w.id, bounds: w.bounds, snapshot: snapshot(w.id) })
        .collect()
}

/// Test only (`STAGE_MANAGER_COVER=desktop`, honoured with `STAGE_MANAGER_TEST` set): a
/// switch's cover is the desktop's still even where no icon lies under the corner, and the
/// still is kept fresh, so the capture phase sees the still-based cover on any desktop
/// (round 8L4).
pub(in crate::platform::controller) fn cover_desktop_forced() -> bool {
    static FORCED: OnceLock<bool> = OnceLock::new();
    *FORCED.get_or_init(|| {
        std::env::var_os("STAGE_MANAGER_TEST").is_some() && std::env::var("STAGE_MANAGER_COVER").is_ok_and(|v| v == "desktop")
    })
}

impl Controller {
    /// Whether switch cover `c` can be drawn now ([`resolve_cover`]): the patch's picture or
    /// the desktop's still (round 8L3), whichever is ready; the cover as it will be drawn, or
    /// why not.
    pub(in crate::platform::controller) fn cover_ready(&self, c: SwitchCover) -> Result<SwitchCover, &'static str> {
        let screen = c.screen;
        let key = self.model.as_ref().and_then(|m| m.displays().iter().find(|d| d.id.0 == screen)).map(|d| self.shot_key(d));
        resolve_cover(c, |rect| crate::ui::corner_patch::cover_ready(screen, rect), |rect| match &key {
            // A still that holds the icons only is laid over the patch's picture (8L4).
            Some(key) => desktop_shot::ready(screen, rect, key, |r| crate::ui::corner_patch::cover_ready(screen, r)),
            None => Err("no model"),
        })
    }

    /// What a still of the desktop in the park corner of display `d` is of now
    /// ([`desktop_shot::Key`]): the Space count, the display, Finder's icons as last read.
    pub(in crate::platform::controller) fn shot_key(&self, d: &Display) -> desktop_shot::Key {
        desktop_shot::Key::new(self.space_changes, d.frame, d.corner, self.patch_items.known.as_deref())
    }

    /// The park corner a switch's overlay covers while its incoming windows (`arriving`) wait
    /// there behind their snapshots ([`crate::engine::corner::corner_patch::switch_cover`], round
    /// 8L): macOS has raised an activated app's windows above the corner patch, and the
    /// switch's focus raises its key window there too. `list` is the window server's list now;
    /// `outgoing` the windows about to park. A window that stays shown into the corner is drawn
    /// on the cover from the strip's snapshot of it (8L5). The reason, and the windows it
    /// names, when the corner cannot be covered (it is left as it is).
    pub(in crate::platform::controller) fn switch_corner(
        &self,
        arriving: &HashSet<u32>,
        outgoing: &HashSet<u32>,
        activated: &HashSet<u32>,
        list: &[windows::CgWindow],
    ) -> Result<SwitchCover, Refused> {
        use crate::engine::corner::corner_patch::{CoverFacts, switch_cover};
        if arriving.is_empty() {
            return Err("nothing arrives".into());
        }
        let m = self.model.as_ref().ok_or("no model")?;
        let config = self.store.config();
        // The screen the switch plays on: the arriving windows' (round displays-2).
        let display = arriving.iter().find_map(|id| m.window(WindowId(*id)).map(|w| m.display(w.meta.display))).unwrap_or(m.main_display());
        let own = std::process::id() as i32;
        // Parked where the window server lists them in the park zone; one the model has parked
        // listed elsewhere shows there and stays shown through the switch (8L6).
        let in_zone = |b: Rect| self.engine.is_hidden(display, b);
        let parked = parked_in_zone(list, m.windows().filter(|w| w.parked).map(|w| w.meta.id.0), in_zone);
        let normal = |w: &&windows::CgWindow| w.layer == 0 && w.alpha > 0.0 && w.pid != own;
        let zone: Vec<&windows::CgWindow> = list
            .iter()
            .filter(normal)
            .filter(|w| !outgoing.contains(&w.id) && (arriving.contains(&w.id) || parked.contains(&w.id)) && in_zone(w.bounds))
            .collect();
        let slivers = zone.iter().map(|w| w.bounds).collect();
        let raised = raised_windows(list, own, arriving, activated, in_zone);
        let staying = staying_windows(list, own, outgoing, arriving, &parked, strip_model::has_snapshot);
        let now = clock::now();
        let facts = CoverFacts {
            on: config.windows.corner_patch,
            screen: display.id.0,
            overview: self.overview.showing_or_expected(now) || self.phase.changing(),
            fullscreen: self.fullscreen_on(display.id),
            slivers,
            raised,
            display: display.frame,
            corner: display.corner,
            visible: display.visible,
            dock_bars: self.dock_bar.as_ref().map(|(_, b)| b.bars.clone()).filter(|b| !b.is_empty()),
            reach: config.windows.patch_reach(),
            in_use: self.corner.patches.get(&display.id.0).and_then(|t| t.rect),
            items: self.patch_items.known.clone(),
            staying,
        };
        let forced = cover_desktop_forced();
        switch_cover(&facts).map(|c| if forced { SwitchCover { picture: CoverPicture::Desktop, ..c } } else { c })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(id: u32, pid: i32, bounds: Rect) -> windows::CgWindow {
        windows::CgWindow { id, pid, owner: String::new(), name: None, layer: 0, bounds, onscreen: true, alpha: 1.0 }
    }

    #[test]
    fn a_corner_with_icons_is_covered_by_the_desktops_still_never_the_wallpaper() {
        // 8L3: the owner's corner has desktop icons; the patch's picture would hide them.
        let rect = Rect::new(2499.0, 1348.0, 61.0, 92.0);
        let cap = Rect::new(2240.0, 1120.0, 320.0, 320.0);
        let cover = |picture| SwitchCover { screen: 1, rect, cap, picture, over: vec![(9, Rect::new(2000.0, 1000.0, 560.0, 440.0))] };
        let ok = |_: Rect| Ok(());
        let no = |_: Rect| Err("not ready");
        // Icons there: the still, or nothing (the activation then arrives at once).
        assert_eq!(resolve_cover(cover(CoverPicture::Desktop), ok, ok), Ok(cover(CoverPicture::Desktop)));
        assert_eq!(resolve_cover(cover(CoverPicture::Desktop), ok, no), Err("not ready"));
        // Nothing there: the patch's picture first, the still when that is not ready.
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), ok, ok), Ok(cover(CoverPicture::Wallpaper)));
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), |_| Err("an aerial"), ok), Ok(cover(CoverPicture::Desktop)));
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), |_| Err("an aerial"), no), Err("an aerial"));
    }

    #[test]
    fn only_windows_that_really_stay_shown_can_keep_a_switchs_corner_uncovered() {
        // 8L5: the owner's windows nearly fill the screen. The outgoing stage's (parking), the
        // arriving ones, the parked ones, ours, a hidden or non-normal one never count; a
        // pinned or unmanaged window does, with whether a snapshot of it is at hand.
        let own = 500;
        let win = |id: u32, pid: i32, layer: i64, alpha: f64| windows::CgWindow {
            id,
            pid,
            owner: String::new(),
            name: None,
            layer,
            bounds: Rect::new(0.0, 0.0, 2560.0, 1410.0),
            onscreen: true,
            alpha,
        };
        let list = [
            win(1, 10, 0, 1.0), // the outgoing stage's, filling the screen
            win(2, 10, 0, 1.0), // its other window
            win(3, 30, 0, 1.0), // arriving (raised in the corner by the activation)
            win(4, 30, 0, 1.0), // parked in another stage
            win(5, own, 0, 1.0), // the corner patch, the overlay
            win(6, 40, 3, 1.0), // a floating panel, not a normal window
            win(7, 41, 0, 0.0), // drawn transparent
            win(8, 50, 0, 1.0), // pinned, snapshot kept
            win(9, 60, 0, 1.0), // unmanaged (the owner's), no snapshot
        ];
        let staying = staying_windows(&list, own, &[1, 2].into(), &[3].into(), &[1, 2, 4].into(), |id| id == 8);
        assert_eq!(
            staying.iter().map(|w| (w.id, w.snapshot)).collect::<Vec<_>>(),
            [(8, true), (9, false)],
            "front to back, as listed"
        );
        // The outgoing windows never count, even when the model has not marked them parked.
        let staying = staying_windows(&list[..2], own, &[1, 2].into(), &HashSet::new(), &HashSet::new(), |_| false);
        assert!(staying.is_empty());
    }

    #[test]
    fn a_window_the_model_has_parked_counts_as_parked_only_where_it_is_listed_in_the_zone() {
        // 8L6, the owner's log: window 101 (its stage parked) listed at its restore frame 533,184
        // 1741x1173 grew the patch's rectangle and every switch's cover to 473,124 2087x1316.
        let (display, visible) = (Rect::new(0.0, 0.0, 2560.0, 1440.0), Rect::new(0.0, 33.0, 2560.0, 1407.0));
        let list = [
            listed(96, 760, Rect::new(2559.0, 1408.0, 1794.0, 1251.0)), // parked, the corner
            listed(115, 795, Rect::new(2559.0, 1399.0, 1894.0, 1336.0)), // parked, the corner
            listed(101, 784, Rect::new(533.0, 184.0, 1741.0, 1173.0)),  // parked in the model's eyes
            listed(109, 746, Rect::new(263.0, 43.0, 2208.0, 1271.0)),   // shown, not parked
        ];
        let parked = parked_in_zone(&list, [96, 115, 101], |b| crate::engine::corner::park::is_parked_in_at(b, display, visible, Corner::BottomRight));
        assert_eq!(parked, [96, 115].into());
        // The patch's rectangle from those: the corner's, never the screen's.
        let slivers: Vec<Rect> = list.iter().filter(|w| parked.contains(&w.id)).map(|w| w.bounds).collect();
        let rect = crate::engine::corner::corner_patch::patch_rect(&slivers, display, crate::engine::corner::corner_patch::DEFAULT_REACH).expect("slivers");
        assert!(crate::engine::corner::corner_patch::cover_cap(display, Corner::BottomRight).contains_rect(&rect), "{rect:?}");
        // And 101 stays shown through a switch: the cover draws it or is not drawn.
        let staying = staying_windows(&list, 500, &[109].into(), &[96].into(), &parked, |_| false);
        assert_eq!(staying.iter().map(|w| w.id).collect::<Vec<_>>(), [101]);
    }

    #[test]
    fn an_activated_apps_window_counts_as_raised_wherever_macos_put_it() {
        // 8L6: the cover must hide what macOS raised; an activated app's window brought into
        // view counts (and then cannot be hidden in the corner); a card click's arriving
        // window outside the zone is not raised by macOS.
        let (display, visible) = (Rect::new(0.0, 0.0, 2560.0, 1440.0), Rect::new(0.0, 33.0, 2560.0, 1407.0));
        let in_zone = |b: Rect| crate::engine::corner::park::is_parked_in_at(b, display, visible, Corner::BottomRight);
        let corner = Rect::new(2559.0, 1408.0, 1794.0, 1251.0);
        let into_view = Rect::new(492.0, 71.0, 1794.0, 1251.0);
        let list = [listed(96, 760, corner), listed(97, 760, into_view), listed(98, 761, into_view), listed(99, 761, corner)];
        let arriving: HashSet<u32> = [96, 97, 98].into();
        assert_eq!(raised_windows(&list, 500, &arriving, &[96, 97].into(), in_zone), [corner, into_view]);
        assert_eq!(raised_windows(&list, 500, &arriving, &HashSet::new(), in_zone), [corner], "a card click");
    }
}
